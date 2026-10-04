//! `sluice me` and the `step_context` tool (SPEC §9): where a step stands, for
//! the agent doing it — one compact answer instead of a pasted note.
use crate::{cli::bad_request, modes::ModeFuture};
use rusqlite::Connection;
use serde_json::{Value, json};
use sluice_model::{
    commands::{MessageView, StepStatus},
    error::PublicError,
    ids::{ProjectId, ProjectSelector, RunId, StepId},
    plan::{Plan, effective_inputs},
    types::Type,
};
use sluice_runtime::{
    dispatch::Catalog,
    registry::{FnRegistry, Registry},
};
use sluice_store::{ReadPool, messages, plans, resources};
use std::path::{Path, PathBuf};

macro_rules! println {
    ($($arg:tt)*) => {
        crate::cli::out(::std::format_args!($($arg)*))
    };
}

const BRIEF: usize = 200;
const FINAL_CUT: usize = 300;

pub fn run(mode: crate::cli::Mode, home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let crate::cli::Mode::Me {
            project,
            step,
            json,
        } = mode
        else {
            return Err(PublicError::not_implemented(mode.name()));
        };
        crate::cli::ensure_home(&home)?;
        let selector = match project.or_else(project_env) {
            Some(value) => value
                .parse::<ProjectSelector>()
                .map_err(|e: sluice_model::ids::InvalidId| bad_request(e.to_string()))?,
            None => {
                return Err(bad_request(
                    "not inside a step (SLUICE_PROJECT_ID/SLUICE_PROJECT are not set); \
                     pass --project and --step",
                ));
            }
        };
        let Some(step) = step.or_else(|| std::env::var("SLUICE_STEP").ok()) else {
            return Err(bad_request(
                "not inside a step (SLUICE_STEP is not set); pass --project and --step",
            ));
        };
        let step: StepId = step
            .parse()
            .map_err(|e: sluice_model::ids::InvalidId| bad_request(e.to_string()))?;
        let run = std::env::var("SLUICE_RUN_ID")
            .ok()
            .and_then(|run| run.parse::<RunId>().ok());
        let context = context(&home, &selector, &step, run).await?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&context).map_err(storage)?
            );
        } else {
            println!("{}", render(&context));
        }
        Ok(())
    })
}

/// Project identity from the environment: SLUICE_PROJECT_ID first (the durable
/// id), SLUICE_PROJECT only as the launch-time display name.
fn project_env() -> Option<String> {
    std::env::var("SLUICE_PROJECT_ID")
        .ok()
        .map(|id| format!("id:{id}"))
        .or_else(|| std::env::var("SLUICE_PROJECT").ok())
        .filter(|value| !value.trim().is_empty())
}
fn storage(error: impl std::fmt::Display) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}

/// The registry's fns as a signature catalog, for plan parsing. Shared with
/// the docs-examples test, which validates plans against the same fns.
pub fn catalog(registry: &Registry) -> Catalog {
    Catalog(
        registry
            .entries()
            .iter()
            .filter_map(|entry| {
                let f = entry.function.as_ref()?;
                Some((
                    f.name.clone(),
                    sluice_model::plan::FnSignature {
                        inputs: f.inputs.clone(),
                        outputs: f.outputs.clone(),
                        submits: f
                            .submits
                            .iter()
                            .map(|(name, ty)| {
                                (
                                    name.clone(),
                                    sluice_model::plan::Declaration {
                                        ty: ty.clone(),
                                        doc: f.submit_docs.get(name).cloned(),
                                    },
                                )
                            })
                            .collect(),
                        open: f.open,
                    },
                ))
            })
            .collect(),
    )
}

fn plan_at(
    sql: &Connection,
    project: ProjectId,
    signatures: &Catalog,
) -> Result<Plan, PublicError> {
    let doc: String = sql
        .query_row(
            "SELECT doc FROM plans WHERE project_id=?1",
            [project.to_string()],
            |r| r.get(0),
        )
        .map_err(sluice_store::StoreError::from)
        .map_err(|e| e.into_public(true))?;
    let document: sluice_model::rpc::JsonMap = sluice_model::rpc::decode_json(doc.as_bytes())?;
    Plan::parse(&document, signatures).map_err(|errors| PublicError::Invalid {
        message: "the stored plan does not validate".into(),
        errors: errors.iter().map(|e| e.to_string()).collect(),
    })
}

/// The step's context: project, step, fn, doc, status (and `queued` for a step
/// held on resources), run times, inputs, upstream steps, unanswered messages,
/// the outputs it must submit and the commands to submit and ask.
pub async fn context(
    home: &Path,
    selector: &ProjectSelector,
    step: &StepId,
    run: Option<RunId>,
) -> Result<Value, PublicError> {
    let reads = ReadPool::open(home, 2).map_err(|e| e.into_public(true))?;
    let project = reads
        .snapshot({
            let selector = selector.clone();
            move |sql| sluice_store::projects::resolve(sql, &selector).map(|p| p.project_id)
        })
        .await
        .map_err(|e| e.into_public(true))?;
    let registry = FnRegistry::configured(home)?.registry(Some(project));
    let signatures = catalog(&registry);
    let step_id = step.clone();
    let home_dir = home.to_path_buf();
    reads
        .snapshot(move |sql| build(sql, &home_dir, project, &signatures, &step_id, run))
        .await
        .map_err(|e| e.into_public(true))
}

fn build(
    sql: &Connection,
    home: &Path,
    project: ProjectId,
    signatures: &Catalog,
    step: &StepId,
    run: Option<RunId>,
) -> Result<Value, sluice_store::StoreError> {
    let plan = plan_at(sql, project, signatures).map_err(sluice_store::StoreError::from)?;
    let state = plans::read_state(sql, project)?;
    let name: String = sql.query_row(
        "SELECT name FROM projects WHERE project_id=?1",
        [project.to_string()],
        |r| r.get(0),
    )?;
    let Some(declaration) = plan.steps().get(step) else {
        return Err(PublicError::NotFound {
            message: format!("the plan of project {name} has no step {step:?}"),
        }
        .into());
    };
    let entry = state.steps.get(step).cloned().unwrap_or_default();
    let status = entry.status.clone();
    let run_ids: Vec<String> = sql
        .query_row(
            "SELECT run_ids FROM steps WHERE project_id=?1 AND step_id=?2",
            rusqlite::params![project.to_string(), step.as_str()],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|ids| serde_json::from_str(&ids).ok())
        .unwrap_or_default();
    let last = run_ids.last().and_then(|id| id.parse::<RunId>().ok());
    // The step's run is the environment's when set; from outside a run, a lone
    // run_id identifies it (several leave `run` null, like the Python context).
    let reported = run.or_else(|| (run_ids.len() == 1).then_some(last).flatten());
    let times = last.and_then(|id| {
        sql.query_row(
            "SELECT started_at,finished_at FROM runs WHERE run_id=?1",
            [id.to_string()],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .ok()
    });
    let (started, finished) = times.unwrap_or((None, None));
    let elapsed = started.as_deref().and_then(|at| {
        let started = parse_time(at)?;
        let end = finished
            .as_deref()
            .and_then(parse_time)
            .unwrap_or_else(now_time);
        Some((end - started).max(0))
    });
    // Resolved inputs: the run's frozen input.json first, else the effective
    // inputs the gate would compute now (absent keys mean it still waits).
    let mut inputs = reported
        .as_ref()
        .and_then(|run| {
            let path = home
                .join("projects")
                .join(project.to_string())
                .join("runs")
                .join(run.to_string())
                .join("input.json");
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        })
        .filter(|value| value.is_object());
    if inputs.is_none() {
        inputs = effective_inputs(&plan, &state, declaration).map(|map| {
            Value::Object(
                map.iter()
                    .map(|(name, input)| {
                        let value = match input {
                            sluice_model::hash::EffectiveInput::Value(v) => v.as_value().clone(),
                            sluice_model::hash::EffectiveInput::File(p) => json!(p),
                        };
                        (name.clone(), value)
                    })
                    .collect::<serde_json::Map<_, _>>(),
            )
        });
    }
    // Queued on resources, like status: a pending step whose needs do not fit.
    let queued = if status == StepStatus::Pending && !declaration.needs.is_empty() {
        resources::admit_order(sql, project, &plan)?
            .into_iter()
            .find(|candidate| &candidate.step == step)
            .and_then(|candidate| {
                resources::fits(sql, project, &candidate.needs)
                    .ok()
                    .filter(|fit| !fit.fits())
                    .map(|fit| format!("queued: {}", fit.reason))
            })
    } else {
        None
    };
    let leases: Vec<Value> = resources::leases(sql, project)?
        .into_iter()
        .filter(|lease| lease.step.as_ref() == Some(step))
        .map(|lease| {
            json!({
                "resource": lease.resource,
                "amount": lease.amount,
                "held": matches!(lease.state, sluice_model::commands::LeaseState::Held),
            })
        })
        .collect();
    let upstream: Vec<Value> = plan
        .dependencies(step)
        .iter()
        .map(|dep| {
            let dep_state = state.steps.get(dep);
            let mut row = json!({
                "step": dep.as_str(),
                "status": dep_state.map(|s| status_name(&s.status)).unwrap_or("pending"),
            });
            if let Some(dep_step) = plan.steps().get(dep) {
                row["fn"] = json!(dep_step.run);
            }
            if let Some(outputs) = dep_state.map(|s| &s.outputs).filter(|o| !o.0.is_empty()) {
                let short: serde_json::Map<String, Value> = outputs
                    .0
                    .iter()
                    .map(|(name, v)| (name.clone(), v.as_value().clone()))
                    .collect();
                row["outputs"] = json!(short_outputs(&short));
            }
            if let Some(error) = dep_state.and_then(|s| s.error.as_ref()) {
                row["error"] = json!(last_line(error));
            }
            row
        })
        .collect();
    let thread = format!("step-{}", step.as_str());
    let open = messages::messages(
        sql,
        project,
        MessageView::Questions,
        Some(thread.as_str()),
        None,
        "cli",
    )?;
    let messages_json: Vec<Value> = open
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .map_err(storage_error)?;
    let mut outputs: Vec<Value> = declaration
        .declared_outputs
        .iter()
        .map(|(name, decl)| {
            let ty = type_text(&decl.ty);
            let mut row = json!({
                "name": name,
                "type": ty,
                "required": !matches!(decl.ty, Type::Optional(_)),
            });
            if let Some(doc) = &decl.doc {
                row["doc"] = json!(doc);
            }
            row
        })
        .collect();
    outputs.sort_by_key(|o| !o["required"].as_bool().unwrap_or(false));
    let args = json!({
        "project": format!("id:{project}"),
        "step": step.as_str(),
        "run": reported.as_ref().map(|r| r.to_string()).unwrap_or_else(|| "<run>".into()),
        "outputs": outputs
            .iter()
            .map(|o| {
                (
                    o["name"].as_str().unwrap_or_default().to_string(),
                    Value::String(format!("<{}>", o["type"].as_str().unwrap_or("?"))),
                )
            })
            .collect::<serde_json::Map<_, _>>(),
    });
    let ask = json!({
        "name": "message.post",
        "project": format!("id:{project}"),
        "direct": true,
        "inputs": {"thread": thread, "from": step.as_str(), "to": "orchestrator", "body": "..."},
    });
    let mut context = json!({
        "project": {"project_id": project.to_string(), "name": name},
        "step": step.as_str(),
        "fn": declaration.run,
        "doc": declaration.doc,
        "status": status_name(&status),
        "started": started,
        "finished": finished,
        "elapsed": elapsed,
        "run": reported.map(|r| r.to_string()),
        "inputs": inputs.map(brief),
        "upstream": upstream,
        "messages": messages_json,
        "submit": {
            "outputs": outputs,
            "command": format!("sluice tool step_submit '{args}'"),
        },
        "thread": thread,
        "ask": format!("sluice tool fn_call '{ask}'"),
    });
    if let Some(queued) = queued {
        context["queued"] = json!(queued);
    }
    if !declaration.needs.is_empty() {
        context["needs"] = serde_json::to_value(&declaration.needs).map_err(storage_error)?;
    }
    if !leases.is_empty() {
        context["leases"] = json!(leases);
    }
    Ok(context)
}

fn storage_error(e: serde_json::Error) -> sluice_store::StoreError {
    sluice_store::StoreError::from(PublicError::Storage {
        message: e.to_string(),
    })
}
fn status_name(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Succeeded => "succeeded",
        StepStatus::Failed => "failed",
        StepStatus::Stale => "stale",
        StepStatus::Skipped => "skipped",
    }
}
fn type_text(ty: &Type) -> String {
    match ty.form() {
        Value::String(s) => s,
        other => other.to_string(),
    }
}
/// The upstream outputs an agent reads first: `summary` whole, else `final`
/// cut, plus every output whose name ends in `report` or `path`.
fn short_outputs(outputs: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    if let Some(summary) = outputs.get("summary") {
        out.insert("summary".into(), summary.clone());
    } else if let Some(value) = outputs.get("final") {
        let value = match value.as_str() {
            Some(text) if text.chars().count() > FINAL_CUT => {
                json!(format!(
                    "{}… [{} more characters]",
                    text.chars().take(FINAL_CUT).collect::<String>(),
                    text.chars().count() - FINAL_CUT
                ))
            }
            _ => value.clone(),
        };
        out.insert("final".into(), value);
    }
    for (name, value) in outputs {
        if !out.contains_key(name) && (name.ends_with("report") || name.ends_with("path")) {
            out.insert(name.clone(), value.clone());
        }
    }
    out
}
fn last_line(error: &str) -> String {
    let line = error.trim().lines().last().unwrap_or("").trim();
    line.chars().take(BRIEF).collect()
}
/// A value with every string over BRIEF characters cut to its start and a note
/// of how much more there is (the status brief cut).
fn brief(value: Value) -> Value {
    match value {
        Value::String(text) if text.chars().count() > BRIEF => json!(format!(
            "{}… [{} more characters]",
            text.chars().take(BRIEF).collect::<String>(),
            text.chars().count() - BRIEF
        )),
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, brief(v))).collect()),
        Value::Array(list) => Value::Array(list.into_iter().map(brief).collect()),
        other => other,
    }
}
fn parse_time(at: &str) -> Option<i64> {
    let (date, time) = at.strip_suffix('Z')?.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da) = (
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
    );
    let mut t = time.split(':');
    let (h, mi, se) = (
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<f64>().ok()?,
    );
    let days = days_from_civil(y, mo, da);
    Some(days * 86400 + h * 3600 + mi * 60 + se as i64)
}
fn now_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
/// Days since the epoch (Howard Hinnant's civil-from-days inverse).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
/// `5s`, `14m`, `2h 3m`, `1d 4h` — how long the step has run.
fn duration(secs: i64) -> String {
    let d = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let mut parts = vec![];
    if d > 0 {
        parts.push(format!("{d}d"));
    }
    if h > 0 {
        parts.push(format!("{h}h"));
    }
    if m > 0 {
        parts.push(format!("{m}m"));
    }
    if d == 0 && h == 0 && (s > 0 || parts.is_empty()) {
        parts.push(format!("{s}s"));
    }
    parts.join(" ")
}

/// The context as short text for an agent at a checkpoint.
pub fn render(context: &Value) -> String {
    let mut head = format!(
        "step {} ({}) — {}",
        context["step"].as_str().unwrap_or("?"),
        context["fn"].as_str().unwrap_or("?"),
        context["status"].as_str().unwrap_or("?"),
    );
    if let Some(elapsed) = context["elapsed"].as_i64() {
        head.push_str(&format!(" {}", duration(elapsed)));
    }
    if let Some(run) = context["run"].as_str() {
        head.push_str(&format!(" · run {run}"));
    }
    let mut lines = vec![head];
    if let Some(queued) = context["queued"].as_str() {
        lines.push(queued.to_string());
    }
    if let Some(leases) = context["leases"].as_array() {
        for lease in leases {
            lines.push(format!(
                "lease: {} {} ({})",
                lease["resource"].as_str().unwrap_or("?"),
                lease["amount"],
                if lease["held"].as_bool().unwrap_or(false) {
                    "held"
                } else {
                    "waiting"
                },
            ));
        }
    }
    if let Some(doc) = context["doc"].as_str() {
        lines.push(format!("doc: {doc}"));
    }
    if let Some(inputs) = context["inputs"].as_object()
        && !inputs.is_empty()
    {
        lines.push(format!(
            "inputs: {}",
            inputs
                .iter()
                .map(|(k, v)| format!("{k}={}", compact(v)))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if let Some(upstream) = context["upstream"].as_array() {
        for u in upstream {
            let mut row = format!(
                "upstream {} ({}): {}",
                u["step"].as_str().unwrap_or("?"),
                u["fn"].as_str().unwrap_or("?"),
                u["status"].as_str().unwrap_or("?"),
            );
            if let Some(outputs) = u["outputs"].as_object()
                && !outputs.is_empty()
            {
                row.push_str(&format!(
                    " — {}",
                    outputs
                        .iter()
                        .map(|(k, v)| format!("{k}={}", compact(v)))
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
            }
            if let Some(error) = u["error"].as_str() {
                row.push_str(&format!(" — error: {error}"));
            }
            lines.push(row);
        }
    }
    if let Some(messages) = context["messages"].as_array() {
        for m in messages {
            let body: String = m["body"]
                .as_str()
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(400)
                .collect();
            lines.push(format!(
                "message {}: {body}",
                m["from"].as_str().unwrap_or("?")
            ));
        }
    }
    if let Some(outputs) = context["submit"]["outputs"].as_array()
        && !outputs.is_empty()
    {
        lines.push(format!(
            "submit: {}",
            outputs
                .iter()
                .map(|o| format!(
                    "{}{}",
                    o["name"].as_str().unwrap_or("?"),
                    if o["required"].as_bool().unwrap_or(false) {
                        ""
                    } else {
                        " (optional)"
                    },
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        lines.push(format!(
            "  {}",
            context["submit"]["command"].as_str().unwrap_or("")
        ));
    }
    lines.push(format!(
        "thread {} — ask: {}",
        context["thread"].as_str().unwrap_or("?"),
        context["ask"].as_str().unwrap_or("?"),
    ));
    lines.join("\n")
}
fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}
