//! Remaining coordinator commands. Reads use short snapshots; waits hold no SQLite lease.
use crate::{
    calls::public,
    coordinator::{Coordinator, cached_context},
    execution::ExecutionHost,
};
use indexmap::IndexMap;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{Value, json};
use sluice_model::{
    commands::*,
    error::PublicError,
    events::Event,
    gates::{self, CachedResources},
    ids::*,
    plan::Binding,
    recipe::{Recipe, RecipeEntry},
    rpc::JsonMap,
    types::{BoundValue, Type, check_value_at},
};
use sluice_store::{RetrySafety, messages, plans, projects, records, resources, writer::ChangeKey};
use std::{path::Path, time::Duration};

#[path = "dispatch_ext/plan_tools.rs"]
mod plan_tools;
#[path = "dispatch_ext/plan_values.rs"]
pub mod plan_values;

fn storage(error: impl std::fmt::Display) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}
fn data(value: impl Serialize) -> Result<CommandReply, PublicError> {
    Ok(CommandReply::Data(
        serde_json::to_value(value).map_err(storage)?.try_into()?,
    ))
}
fn bad(message: impl Into<String>) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}

/// Global files first; project files shadow them even when broken. Discovery is sorted.
/// Called on a blocking snapshot thread, never on the SQLite writer.
pub(crate) fn load_recipes(
    home: &Path,
    project: ProjectId,
) -> sluice_store::Result<IndexMap<String, RecipeEntry>> {
    let mut entries = IndexMap::new();
    for (scope, directory) in [
        ("global", home.join("recipes")),
        (
            "project",
            home.join("projects")
                .join(project.to_string())
                .join("recipes"),
        ),
    ] {
        let files = match std::fs::read_dir(directory) {
            Ok(files) => files,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let mut paths = files
            .map(|file| file.map(|file| file.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        for path in paths
            .into_iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        {
            let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            let recipe = match std::fs::read(&path) {
                Ok(bytes) => Recipe::parse_json(name, &bytes),
                Err(error) => Err(vec![sluice_model::types::PathError {
                    path: "recipe".into(),
                    message: error.to_string(),
                }]),
            };
            entries.insert(
                name.to_owned(),
                RecipeEntry {
                    name: name.to_owned(),
                    scope: scope.into(),
                    recipe,
                },
            );
        }
    }
    entries.sort_keys();
    Ok(entries)
}
fn recipe_list(entries: IndexMap<String, RecipeEntry>) -> Value {
    json!(entries.values().map(|entry| match &entry.recipe {
        Ok(recipe) => {
            let mut out = json!({"name":entry.name,"scope":entry.scope,"doc":recipe.doc(),"params":recipe.params().iter().map(|(name,decl)| (name.clone(), if let Some(doc)=&decl.doc {json!({"type":decl.ty,"doc":doc})} else {json!(decl.ty)})).collect::<serde_json::Map<_,_>>(),"stages":recipe.stages()});
            // a title or view that does not check is reported, and the recipe still adds units
            for (key, source, error) in [
                ("title", recipe.title_source(), recipe.title_error()),
                ("view", recipe.view_source(), recipe.view_error()),
            ] {
                if let Some(source) = source {
                    out[key] = json!(source);
                }
                if let Some(error) = error {
                    out[format!("{key}_error")] = json!(error);
                }
            }
            out
        }
        Err(errors) => json!({"name":entry.name,"scope":entry.scope,"error":errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")}),
    }).collect::<Vec<_>>())
}

/// `None` leaves already-owned variants to the main dispatch table.
pub async fn dispatch_ext<H: ExecutionHost>(
    broker: &Coordinator<H>,
    request: CommandRequest,
) -> Result<Option<CommandReply>, PublicError> {
    if let Some(reply) = plan_tools::dispatch(broker, request.clone()).await? {
        return Ok(Some(reply));
    }
    let catalog = broker.catalog().clone();
    let reply = match request {
        CommandRequest::ProjectDelete(request) => {
            let deleted = broker
                .writer()
                .write(RetrySafety::NonIdempotent, move |tx| {
                    projects::project_delete(
                        tx,
                        &request.project,
                        projects::DeleteProject {
                            confirm_name: request.confirm_name.to_string(),
                            expected_settings_rev: request.expected_settings_rev,
                            author: request.author.unwrap_or_else(|| "cli".into()),
                        },
                        &projects::StoredWorkOnly,
                    )
                })
                .await?;
            sluice_store::artifacts::recover(broker.writer(), broker.home())
                .await
                .map_err(public)?;
            CommandReply::Deleted {
                project_id: deleted.project_id,
                name: deleted.name,
                deleted: true,
            }
        }
        CommandRequest::StepDismiss(request) => {
            broker
                .writer()
                .write(RetrySafety::Idempotent, move |tx| {
                    messages::dismiss(tx, request)
                })
                .await?;
            CommandReply::Ack
        }
        CommandRequest::MarkRead(request) => {
            broker
                .writer()
                .write(RetrySafety::Idempotent, move |tx| {
                    messages::mark_read(tx, request)
                })
                .await?;
            CommandReply::Ack
        }
        CommandRequest::RecipeList { project } => {
            let home = broker.home().to_owned();
            data(
                broker
                    .reads()
                    .snapshot(move |sql| {
                        let id = messages::resolve_project(sql, &project)?;
                        Ok(recipe_list(load_recipes(&home, id)?))
                    })
                    .await
                    .map_err(public)?,
            )?
        }
        CommandRequest::StepContext { project, step } => {
            let cache = broker.plan_cache();
            let (mut out, note) = broker
                .reads()
                .snapshot(move |sql| {
                    let id = messages::resolve_project(sql, &project)?;
                    let ctx = cached_context(sql, id, &catalog, &cache)?;
                    step_context(sql, &ctx, &step)
                })
                .await
                .map_err(public)?;
            if let Some(mut note) = note {
                note.worktree =
                    sluice_agents::git::worktree_of(out["inputs"]["cwd"].as_str()).await;
                out["attempt"] = attempt_value(&note).map_err(storage)?;
            }
            data(out)?
        }
        CommandRequest::Query(request) => {
            let home = broker.home().to_owned();
            let params = request
                .params
                .iter()
                .map(|value| sql_value(value.as_value()))
                .collect::<Result<Vec<_>, _>>()?;
            let value = tokio::task::spawn_blocking(move || {
                let result = sluice_store::query::query(
                    &home,
                    &request.sql,
                    Some(&params),
                    Some(request.limit as usize),
                )?;
                serde_json::from_slice::<Value>(result.encoded()).map_err(storage)
            })
            .await
            .map_err(storage)??;
            data(value)?
        }
        CommandRequest::LogWait(request) => CommandReply::Records(log_wait(broker, request).await?),
        CommandRequest::StepWait(request) => {
            CommandReply::StepWait(step_wait(broker, request).await?)
        }
        CommandRequest::Docs { topic } => data(crate::docs::docs(topic.as_deref())?)?,
        CommandRequest::PlanSetInput(request) => {
            if request.edit.dry_run {
                // Not a plan edit: a runtime value. Its preview is the whole plan simulated
                // before and after the value (`scope: all`), from the cached compiled plan.
                let cache = broker.plan_cache();
                let preview = broker
                    .reads()
                    .snapshot(move |sql| {
                        let id = messages::resolve_project(sql, &request.project)?;
                        let ctx = cached_context(sql, id, &catalog, &cache)?;
                        if request.edit.expected.is_some_and(|rev| rev != ctx.revision) {
                            return Err(PublicError::Conflict {
                                message: "plan revision changed".into(),
                                current_rev: Some(ctx.revision),
                            }
                            .into());
                        }
                        let declaration = ctx
                            .plan
                            .inputs()
                            .get(&request.name)
                            .ok_or_else(|| bad("no such plan input"))?;
                        check_value_at(
                            &declaration.ty,
                            request.value.as_value(),
                            &format!("inputs.{}", request.name),
                        )
                        .map_err(|errors| PublicError::Invalid {
                            message: "invalid input".into(),
                            errors: errors.into_iter().map(|e| e.to_string()).collect(),
                        })?;
                        let before = plans::read_state(sql, id)?;
                        crate::models::check_input(
                            &ctx.plan,
                            &request.name,
                            request.value.as_value(),
                            &before.inputs,
                        )?;
                        let mut after = before.clone();
                        after.inputs.0.insert(request.name, request.value);
                        let simulated = gates::simulate_edit(
                            &ctx.plan,
                            &before,
                            &ctx.plan,
                            &after,
                            &cached_resources(sql, id, ctx.revision, &ctx.plan)?,
                        );
                        Ok(sluice_model::plan_rows::EditPreview {
                            scope: sluice_model::plan_rows::PreviewScope::All,
                            changes: vec![],
                            would_start: simulated.would_start,
                            would_queue: simulated.would_queue.into_keys().collect(),
                            would_skip: simulated.would_skip.into_keys().collect(),
                            would_stale: simulated.would_stale,
                            errors: simulated
                                .errors
                                .into_iter()
                                .map(|e| e.to_string())
                                .collect(),
                        })
                    })
                    .await
                    .map_err(public)?;
                CommandReply::Preview(preview)
            } else {
                return Ok(None);
            }
        }
        CommandRequest::Backup { destination } => {
            let home = broker.home().to_owned();
            let info = tokio::task::spawn_blocking(move || {
                sluice_store::backup::backup(&home, Path::new(&destination))
            })
            .await
            .map_err(storage)?
            .map_err(public)?;
            data(json!({"path":info.path,"bytes":info.bytes}))?
        }
        CommandRequest::Builtin { .. } => {
            return Err(bad(
                "builtin execution requires an authenticated guardian invocation",
            ));
        }
        _ => return Ok(None),
    };
    Ok(Some(reply))
}

fn sql_value(value: &Value) -> Result<rusqlite::types::Value, PublicError> {
    use rusqlite::types::Value as Sql;
    Ok(match value {
        Value::Null => Sql::Null,
        Value::Bool(value) => Sql::Integer(i64::from(*value)),
        Value::String(value) => Sql::Text(value.clone()),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Sql::Integer(value)
            } else {
                Sql::Real(value.as_f64().ok_or_else(|| bad("invalid SQL number"))?)
            }
        }
        _ => {
            return Err(bad(
                "SQL parameters must be null, boolean, number or string",
            ));
        }
    })
}
fn cached_resources(
    sql: &Connection,
    id: ProjectId,
    rev: Revision,
    plan: &sluice_model::plan::Plan,
) -> sluice_store::Result<CachedResources> {
    let mut cached = CachedResources::default();
    let state = plans::read_state(sql, id)?;
    for (name, status) in resources::status(sql, id, rev, plan)? {
        let running_needs: u64 = plan
            .steps()
            .iter()
            .filter(|(id, _)| state.status(id) == StepStatus::Running)
            .fold(0_u64, |held, (_, step)| {
                held.saturating_add(step.needs.get(&name).copied().unwrap_or(0))
            });
        cached
            .leased
            .insert(name.clone(), status.held.saturating_sub(running_needs));
        cached.capacities.insert(name, status.resource.capacity);
    }
    Ok(cached)
}

async fn log_wait<H: ExecutionHost>(
    broker: &Coordinator<H>,
    request: LogWait,
) -> Result<RecordPage, PublicError> {
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(request.timeout_seconds.min(3600));
    let project = crate::calls::resolve(broker.reads(), request.read.project.clone()).await?;
    // Subscribe to this log's durable version before reading: a commit during a
    // snapshot cannot be missed, and other logs' commits never wake the wait.
    let mut changes = broker
        .reads()
        .subscribe(broker.writer(), vec![ChangeKey::new(project, "log")])
        .await
        .map_err(public)?;
    let mut filter = records::RecordFilter::from(&request.read);
    filter.since = filter.since.or(Some(RecordSeq(0)));
    let mut held = vec![];
    loop {
        let remaining = (request.read.limit as usize).saturating_sub(held.len());
        if remaining == 0 {
            // The bounded page is full of notes. Wait for a waking record without
            // consuming it, then return the notes' cursor so the next page sees it.
            let probe = records::RecordFilter {
                limit: 1,
                ..filter.clone()
            };
            let page = broker
                .reads()
                .snapshot(move |sql| records::read_records(sql, project, &probe)?.into_page())
                .await
                .map_err(public)?;
            if page
                .records
                .iter()
                .any(|r| !matches!(&r.event, Event::Message(m) if !m.is_question()))
                || tokio::time::Instant::now() >= deadline
            {
                return Ok(RecordPage {
                    last_seq: held
                        .last()
                        .map_or(page.last_seq, |r: &sluice_model::events::Record| r.seq),
                    records: held,
                });
            }
            filter.since = Some(page.last_seq);
        } else {
            let mut page_filter = filter.clone();
            page_filter.limit = remaining as u32;
            let page = broker
                .reads()
                .snapshot(move |sql| records::read_records(sql, project, &page_filter)?.into_page())
                .await
                .map_err(public)?;
            let wakes = !request.questions_only
                || page
                    .records
                    .iter()
                    .any(|r| !matches!(&r.event, Event::Message(m) if !m.is_question()));
            let has_records = !page.records.is_empty();
            filter.since = Some(page.last_seq);
            held.extend(page.records);
            if (has_records && wakes) || tokio::time::Instant::now() >= deadline {
                return Ok(RecordPage {
                    records: held,
                    last_seq: page.last_seq,
                });
            }
        }
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {},
            changed = changes.wait() => { changed.map_err(public)?; },
        }
    }
}

/// `step_wait`: one reading per commit to the project's log (every status change, pause and
/// plan edit writes a record there), like `log_wait`; the plan is compiled again only when
/// its revision moves.
async fn step_wait<H: ExecutionHost>(
    broker: &Coordinator<H>,
    request: StepWait,
) -> Result<StepWaitResult, PublicError> {
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(request.timeout_seconds.min(3600));
    let invalid = |message: &str, errors: Vec<String>| PublicError::Invalid {
        message: message.into(),
        errors,
    };
    let until = StepWaitUntil::parse(request.until.as_value())
        .map_err(|errors| invalid("step_wait: invalid until", errors))?;
    let steps = request.selection.steps.unwrap_or_default();
    let tags = request.selection.tags.unwrap_or_default();
    match (steps.is_empty(), tags.is_empty()) {
        (true, true) => {
            return Err(invalid(
                "step_wait: select steps by steps or by tags",
                vec!["steps: name at least one step id, or give tags instead".into()],
            ));
        }
        (false, false) => {
            return Err(invalid(
                "step_wait: give steps or tags, not both",
                vec!["tags: leave out when steps is given".into()],
            ));
        }
        _ => {}
    }
    let project = crate::calls::resolve(broker.reads(), Some(request.project))
        .await?
        .ok_or_else(|| bad("step_wait needs a project"))?;
    let mut changes = broker
        .reads()
        .subscribe(broker.writer(), vec![ChangeKey::new(Some(project), "log")])
        .await
        .map_err(public)?;
    let (steps, tags) = (std::sync::Arc::new(steps), std::sync::Arc::new(tags));
    let mut compiled: Option<(Revision, std::sync::Arc<sluice_model::plan::Plan>)> = None;
    loop {
        let (catalog, cache) = (broker.catalog().clone(), broker.plan_cache());
        let (steps, tags, cached) = (steps.clone(), tags.clone(), compiled.clone());
        let until = until.clone();
        let (result, plan) = broker
            .reads()
            .snapshot(move |sql| {
                let rev: i64 = sql.query_row(
                    "SELECT rev FROM plans WHERE project_id=?1",
                    [project.to_string()],
                    |r| r.get(0),
                )?;
                let rev = Revision(rev as u64);
                let plan = match cached {
                    Some((at, plan)) if at == rev => plan,
                    _ => {
                        cache
                            .current(
                                sql,
                                project,
                                catalog.generation(),
                                &catalog.for_project(Some(project)),
                            )?
                            .1
                    }
                };
                let mut errors: Vec<String> = steps
                    .iter()
                    .filter(|id| !plan.steps().contains_key(*id))
                    .map(|id| format!("steps: no step {id}"))
                    .collect();
                errors.extend(
                    tags.iter()
                        .filter(|tag| !plan.steps().values().any(|s| s.tags.contains(*tag)))
                        .map(|tag| format!("tags: no step is tagged {tag}")),
                );
                if !errors.is_empty() {
                    return Err(
                        invalid("step_wait: the selection names no such steps", errors).into(),
                    );
                }
                let state = plans::read_state(sql, project)?;
                let settled = if until == StepWaitUntil::Named(StepWaitTarget::Settled) {
                    sluice_model::units::settled_steps(&plan, &state)
                } else {
                    Default::default()
                };
                let mut met = true;
                let mut statuses = IndexMap::new();
                for (id, step) in plan.steps() {
                    if steps.contains(id) || step.tags.iter().any(|tag| tags.contains(tag)) {
                        let status = state.status(id);
                        met &= until.met(&status, settled.contains(id));
                        statuses.insert(id.clone(), status);
                    }
                }
                let seq = records::bounds(sql, Some(project))?.1;
                Ok((
                    StepWaitResult {
                        met,
                        steps: statuses,
                        seq,
                    },
                    (rev, plan),
                ))
            })
            .await
            .map_err(public)?;
        if result.met || tokio::time::Instant::now() >= deadline {
            return Ok(result);
        }
        compiled = Some(plan);
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {},
            changed = changes.wait() => { changed.map_err(public)?; },
        }
    }
}

fn short(mut value: Value, limit: usize) -> Value {
    match &mut value {
        Value::String(s) if s.chars().count() > limit => {
            let count = s.chars().count();
            *s = format!(
                "{}… [{} more characters]",
                s.chars().take(limit).collect::<String>(),
                count - limit
            );
        }
        Value::Array(a) => {
            for v in a {
                *v = short(v.take(), limit);
            }
        }
        Value::Object(o) => {
            for v in o.values_mut() {
                *v = short(v.take(), limit);
            }
        }
        _ => {}
    }
    value
}
fn upstream_outputs(outputs: &JsonMap) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(value) = outputs.0.get("summary") {
        out.insert("summary".into(), value.as_value().clone());
    } else if let Some(value) = outputs.0.get("final") {
        out.insert("final".into(), short(value.as_value().clone(), 300));
    }
    for (name, value) in &outputs.0 {
        if name.ends_with("report") || name.ends_with("path") {
            out.insert(name.clone(), value.as_value().clone());
        }
    }
    Value::Object(out)
}
fn shell_json(value: &Value) -> String {
    format!("'{}'", value.to_string().replace('\'', "'\\''"))
}
/// `step_context`'s `attempt`: which attempt this is, the one before it and the work tree,
/// with the note the agent's task starts with.
pub fn attempt_value(note: &sluice_model::attempt::AttemptNote) -> serde_json::Result<Value> {
    let mut value = serde_json::to_value(note)?;
    value["note"] = json!(note.text());
    Ok(value)
}
/// The context and, unless the step is scattered, its attempt note without the work tree
/// (the caller adds it: that needs git, outside the read snapshot).
fn step_context(
    sql: &Connection,
    ctx: &plans::PlanContext,
    id: &StepId,
) -> sluice_store::Result<(Value, Option<sluice_model::attempt::AttemptNote>)> {
    let step = ctx
        .plan
        .steps()
        .get(id)
        .ok_or_else(|| PublicError::NotFound {
            message: format!("no step {id}"),
        })?;
    let project = projects::resolve(sql, &ProjectSelector::Id(ctx.project))?;
    let state = plans::read_state(sql, ctx.project)?;
    let run_ids: String = sql.query_row(
        "SELECT run_ids FROM steps WHERE project_id=?1 AND step_id=?2",
        (ctx.project.to_string(), id.as_str()),
        |r| r.get(0),
    )?;
    let runs: Vec<RunId> = serde_json::from_str(&run_ids)?;
    let run = if runs.len() == 1 { Some(runs[0]) } else { None };
    let timing: (Option<String>, Option<String>, Option<f64>) = sql.query_row(
        "SELECT min(started_at),CASE WHEN count(*)=count(finished_at) THEN max(finished_at) END,
         max(0,(julianday(CASE WHEN count(*)=count(finished_at) THEN max(finished_at) ELSE strftime('%Y-%m-%dT%H:%M:%fZ','now') END)-julianday(min(started_at)))*86400)
         FROM runs WHERE project_id=?1 AND run_id IN (SELECT value FROM json_each(?2))",
        (ctx.project.to_string(), &run_ids), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    let frozen: Option<String> = if let Some(run) = run {
        sql.query_row("SELECT json_extract(a.request,'$.inputs') FROM runs r JOIN attempts a USING(attempt_id) WHERE r.run_id=?1 AND r.project_id=?2", (run.to_string(), ctx.project.to_string()), |r| r.get(0)).optional()?.flatten()
    } else {
        None
    };
    let inputs = if let Some(frozen) = frozen {
        serde_json::from_str(&frozen)?
    } else {
        Value::Object(
            step.bindings
                .iter()
                .map(|(name, binding)| {
                    let value = match binding {
                        Binding::Default(value) => value.as_value().clone(),
                        Binding::File(file) => json!({"file":file}),
                        Binding::Source(reference) => {
                            match gates::resolve_reference(&ctx.plan, &state, reference) {
                                BoundValue::Ready(value) => value.into_value(),
                                _ => Value::Null,
                            }
                        }
                        Binding::Sources(references) => json!(
                            references
                                .iter()
                                .map(|reference| {
                                    match gates::resolve_reference(&ctx.plan, &state, reference) {
                                        BoundValue::Ready(value) => value.into_value(),
                                        _ => Value::Null,
                                    }
                                })
                                .collect::<Vec<_>>()
                        ),
                    };
                    (name.clone(), value)
                })
                .collect(),
        )
    };
    let upstream: Vec<_> = ctx.plan.dependencies(id).iter().map(|up| {
        let entry = state.steps.get(up);
        json!({"step":up,"fn":ctx.plan.steps()[up].run,"status":state.status(up),
            "outputs":entry.map(|e| upstream_outputs(&e.outputs)),"error":entry.and_then(|e|e.error.as_ref()).map(|e| short(json!(e), 200))})
    }).collect();
    let thread = format!(
        "step-{}",
        id.as_str()
            .to_lowercase()
            .chars()
            .map(
                |c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                    c
                } else {
                    '-'
                }
            )
            .collect::<String>()
    );
    let open = messages::messages(
        sql,
        ctx.project,
        MessageView::Questions,
        Some(&thread),
        None,
        id.as_str(),
    )?;
    let mut outputs: Vec<_> = step.declared_outputs.iter().map(|(name, decl)| json!({"name":name,"type":decl.ty,"required":!matches!(decl.ty, Type::Optional(_)),"doc":decl.doc})).collect();
    outputs.sort_by_key(|o| o["required"] != true);
    let args = json!({"project":ctx.project,"step":id,"run":run.map_or_else(|| "<run>".into(), |r|r.to_string()),"outputs":outputs.iter().map(|o| (o["name"].as_str().unwrap().to_owned(), json!(format!("<{}>",o["type"].as_str().map(str::to_owned).unwrap_or_else(||o["type"].to_string()))))).collect::<serde_json::Map<_,_>>()});
    let ask = json!({"project":ctx.project,"run":run.map_or_else(|| "<run>".into(), |r|r.to_string()),"to":"orchestrator","body":"..."});
    let mut out = json!({"project":project.name,"project_id":ctx.project,"step":id,"fn":step.run,"doc":step.doc,"status":state.status(id),"started":timing.0,"finished":timing.1,"elapsed":timing.2,"run":run,"inputs":short(inputs,200),"upstream":upstream,"messages":open,"submit":{"outputs":outputs,"command":format!("sluice tool step_submit {}",shell_json(&args)),"note":sluice_agents::prompt::SUBMIT_ENDS_SESSION},"thread":thread,"ask":format!("sluice tool ask {}",shell_json(&ask))});
    if !step.needs.is_empty() {
        out["needs"] = json!(step.needs);
    }
    if resources::admit_order(sql, ctx.project, ctx.revision, &ctx.plan)?
        .iter()
        .any(|candidate| candidate.step == *id)
    {
        let fit = resources::fits(
            sql,
            ctx.project,
            &step.needs.iter().map(|(n, v)| (n.clone(), *v)).collect(),
        )?;
        if !fit.fits() {
            out["queued"] = json!(format!("queued: {}", fit.reason));
        }
    }
    let leases: Vec<_> = resources::leases(sql, ctx.project)?
        .into_iter()
        .filter(|l| l.step.as_ref() == Some(id))
        .map(|l| json!({"resource":l.resource,"amount":l.amount,"held":l.state==LeaseState::Held}))
        .collect();
    if !leases.is_empty() {
        out["leases"] = json!(leases);
    }
    if let Some(finishing) = sluice_store::attempts::finishing(sql, ctx.project)?.remove(id) {
        out["finishing"] = serde_json::to_value(finishing)?;
    }
    let note = if step.scatter.is_none() {
        let current = run.filter(|_| state.status(id) == StepStatus::Running);
        Some(sluice_store::attempts::attempt_note(
            sql,
            ctx.project,
            id,
            current,
        )?)
    } else {
        None
    };
    Ok((out, note))
}

/// `plan_view`'s text for a project, in the caller's read snapshot (plan-rows §7.9): read from
/// the graph index, so no document, compile or fn catalog is needed.
pub fn render_plan_view(
    sql: &Connection,
    home: &std::path::Path,
    project: ProjectId,
    query: sluice_model::plan_rows::PlanViewQuery,
) -> sluice_store::Result<String> {
    plan_tools::plan_view(sql, home, project, query)
}
