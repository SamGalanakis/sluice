//! Remaining coordinator commands. Reads use short snapshots; waits hold no SQLite lease.
use crate::{
    calls::public,
    coordinator::{Coordinator, context},
    execution::ExecutionHost,
};
use indexmap::IndexMap;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{Value, json};
use sluice_model::{
    commands::*,
    edit::{self, EditSnapshot, PlanEdit},
    error::PublicError,
    events::Event,
    gates::{self, CachedResources, Gate},
    ids::*,
    plan::{Binding, Snapshot},
    recipe::{Recipe, RecipeEntry},
    rpc::JsonMap,
    types::{BoundValue, Type, check_value_at},
};
use sluice_store::{RetrySafety, messages, plans, projects, records, resources, writer::ChangeKey};
use std::{path::Path, time::Duration};

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
        Ok(recipe) => json!({"name":entry.name,"scope":entry.scope,"doc":recipe.doc(),"params":recipe.params().iter().map(|(name,decl)| (name.clone(), if let Some(doc)=&decl.doc {json!({"type":decl.ty,"doc":doc})} else {json!(decl.ty)})).collect::<serde_json::Map<_,_>>()}),
        Err(errors) => json!({"name":entry.name,"scope":entry.scope,"error":errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")}),
    }).collect::<Vec<_>>())
}

/// `None` leaves already-owned variants to the main dispatch table.
pub async fn dispatch_ext<H: ExecutionHost>(
    broker: &Coordinator<H>,
    request: CommandRequest,
) -> Result<Option<CommandReply>, PublicError> {
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
        CommandRequest::UnitAdd(request) => {
            edit_extension(broker, PlanEdit::UnitAdd(request)).await?
        }
        CommandRequest::PlanPrune(request) => {
            edit_extension(broker, PlanEdit::PlanPrune(request)).await?
        }
        CommandRequest::PlanHistory { project, since_rev } => data(
            broker
                .reads()
                .snapshot(move |sql| {
                    plans::history(sql, messages::resolve_project(sql, &project)?, since_rev)
                })
                .await
                .map_err(public)?,
        )?,
        CommandRequest::StepContext { project, step } => data(
            broker
                .reads()
                .snapshot(move |sql| {
                    let id = messages::resolve_project(sql, &project)?;
                    let ctx = context(sql, id, &catalog)?;
                    step_context(sql, &ctx, &step)
                })
                .await
                .map_err(public)?,
        )?,
        CommandRequest::PlanView {
            project,
            format,
            all,
        } => data(
            broker
                .reads()
                .snapshot(move |sql| {
                    let id = messages::resolve_project(sql, &project)?;
                    let ctx = context(sql, id, &catalog)?;
                    let state = plans::read_state(sql, id)?;
                    let name = projects::resolve(sql, &ProjectSelector::Id(id))?.name;
                    let mut stmt = sql.prepare("SELECT step_id,manual,done,total FROM steps WHERE project_id=?1 ORDER BY position")?;
                    let metadata = stmt.query_map([id.to_string()], |row| Ok((row.get::<_, String>(0)?, StepViewState { manual: row.get(1)?, done: row.get(2)?, total: row.get(3)? })))?
                        .map(|row| { let (id, metadata) = row?; Ok((crate::calls::parse_id(id)?, metadata)) }).collect::<sluice_store::Result<IndexMap<StepId, StepViewState>>>()?;
                    Ok(plan_view(&name, &ctx.plan, &state, format, all, &metadata))
                })
                .await
                .map_err(public)?,
        )?,
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
        CommandRequest::Docs { topic } => data(crate::docs::docs(topic.as_deref())?)?,
        CommandRequest::PlanSetInput(request) => {
            if request.edit.dry_run {
                let preview = broker
                    .reads()
                    .snapshot(move |sql| {
                        let id = messages::resolve_project(sql, &request.project)?;
                        let ctx = context(sql, id, &catalog)?;
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
                        let mut after = before.clone();
                        after.inputs.0.insert(request.name, request.value);
                        let simulated = gates::simulate_edit(
                            &ctx.plan,
                            &before,
                            &ctx.plan,
                            &after,
                            &cached_resources(sql, id, &ctx.plan)?,
                        );
                        Ok(EditPreview {
                            ops: vec![],
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
    plan: &sluice_model::plan::Plan,
) -> sluice_store::Result<CachedResources> {
    let mut cached = CachedResources::default();
    let state = plans::read_state(sql, id)?;
    for (name, status) in resources::status(sql, id, plan)? {
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

async fn edit_extension<H: ExecutionHost>(
    broker: &Coordinator<H>,
    edit: PlanEdit,
) -> Result<CommandReply, PublicError> {
    let (project, age) = match &edit {
        PlanEdit::UnitAdd(request) => (request.project.clone(), None),
        PlanEdit::PlanPrune(request) => (request.project.clone(), Some(request.older_than_seconds)),
        _ => return Err(bad("unsupported extension edit")),
    };
    let catalog = broker.catalog().clone();
    let home = broker.home().to_owned();
    let (id, prepared, evidence) = broker
        .reads()
        .snapshot(move |sql| {
            let id = messages::resolve_project(sql, &project)?;
            let ctx = context(sql, id, &catalog)?;
            let state = plans::read_state(sql, id)?;
            let evidence = age
                .map(|age| plans::prune_eligible_age(sql, &ctx, age))
                .transpose()?;
            let limits = resources::declarations(sql, id)?
                .into_iter()
                .map(|(name, resource)| {
                    (
                        name,
                        match resource.declaration {
                            resources::Capacity::Fixed(n) => {
                                sluice_model::plan::ResourceLimit::Fixed(n)
                            }
                            resources::Capacity::Function(_) => {
                                sluice_model::plan::ResourceLimit::Dynamic
                            }
                        },
                    )
                })
                .collect();
            let prepared = edit::prepare_edit(
                &EditSnapshot {
                    snapshot: &Snapshot {
                        revision: ctx.revision,
                        document: ctx.plan.document().clone(),
                    },
                    state: &state,
                    signatures: &catalog.for_project(Some(id)),
                    recipes: &load_recipes(&home, id)?,
                    resources: &cached_resources(sql, id, &ctx.plan)?,
                    limits: &limits,
                    prune_eligible: evidence.as_ref().map(|e| e.units()),
                },
                edit,
            )?;
            Ok((id, prepared, evidence))
        })
        .await
        .map_err(public)?;
    if prepared.dry_run {
        return Ok(CommandReply::Preview(prepared.preview));
    }
    broker
        .writer()
        .write(RetrySafety::NonIdempotent, move |tx| {
            crate::drain::ensure_edit(tx)?;
            let result = if let Some(evidence) = evidence {
                plans::apply_prune(tx, id, prepared, &evidence)?
            } else {
                plans::apply_edit(tx, id, prepared)?
            };
            Ok(CommandReply::Edit(result))
        })
        .await
}

async fn log_wait<H: ExecutionHost>(
    broker: &Coordinator<H>,
    request: LogWait,
) -> Result<RecordPage, PublicError> {
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(request.timeout_seconds.min(3600));
    let project = crate::calls::resolve(broker.reads(), request.read.project).await?;
    // Subscribe to this log's durable version before reading: a commit during a
    // snapshot cannot be missed, and other logs' commits never wake the wait.
    let mut changes = broker
        .reads()
        .subscribe(broker.writer(), vec![ChangeKey::new(project, "log")])
        .await
        .map_err(public)?;
    let mut filter = records::RecordFilter {
        since: request.read.since_seq.or(Some(RecordSeq(0))),
        kinds: request.read.kinds.unwrap_or_default(),
        threads: request.read.threads.unwrap_or_default(),
        limit: request.read.limit,
    };
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
                .any(|r| !matches!(&r.event, Event::Message(m) if !m.needs_reply))
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
                    .any(|r| !matches!(&r.event, Event::Message(m) if !m.needs_reply));
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
fn step_context(
    sql: &Connection,
    ctx: &plans::PlanContext,
    id: &StepId,
) -> sluice_store::Result<Value> {
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
    let ask = json!({"name":"message.post","project":ctx.project,"direct":true,"inputs":{"thread":thread,"from":id,"to":"orchestrator","body":"..."}});
    let mut out = json!({"project":project.name,"project_id":ctx.project,"step":id,"fn":step.run,"doc":step.doc,"status":state.status(id),"started":timing.0,"finished":timing.1,"elapsed":timing.2,"run":run,"inputs":short(inputs,200),"upstream":upstream,"messages":open,"submit":{"outputs":outputs,"command":format!("sluice tool step_submit {}",shell_json(&args))},"thread":thread,"ask":format!("sluice tool fn_call {}",shell_json(&ask))});
    if !step.needs.is_empty() {
        out["needs"] = json!(step.needs);
    }
    if resources::admit_order(sql, ctx.project, &ctx.plan)?
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
    Ok(out)
}

fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn label(text: &str) -> String {
    html(text)
        .replace('\n', " ")
        .replace('[', "&#91;")
        .replace(']', "&#93;")
}
struct StepViewState {
    manual: bool,
    done: i64,
    total: Option<i64>,
}
fn plan_view(
    name: &ProjectName,
    plan: &sluice_model::plan::Plan,
    state: &gates::StateSnapshot,
    format: PlanViewFormat,
    all: bool,
    metadata: &IndexMap<StepId, StepViewState>,
) -> String {
    let done: Vec<_> = plan
        .units()
        .values()
        .filter(|unit| !all && unit.done(state))
        .collect();
    let visible: Vec<_> = plan
        .topological_order()
        .iter()
        .filter(|id| !done.iter().any(|u| u.steps.contains(id)))
        .collect();
    let nodes: IndexMap<_, _> = visible
        .iter()
        .enumerate()
        .map(|(i, id)| ((*id).clone(), format!("s{i}")))
        .collect();
    let units: IndexMap<_, _> = plan
        .units()
        .iter()
        .filter(|(_, unit)| unit.steps.iter().any(|id| nodes.contains_key(id)))
        .enumerate()
        .map(|(i, (name, _))| (name.clone(), format!("u{i}")))
        .collect();
    let inputs: IndexMap<_, _> = plan
        .inputs()
        .keys()
        .enumerate()
        .map(|(i, name)| (name.clone(), format!("i{i}")))
        .collect();
    let mut diagram = String::from("flowchart LR\n");
    let mut boxes = vec![];
    for (name, node) in &inputs {
        diagram.push_str(&format!("  {node}([\"{}\"])\n", label(name)));
        boxes.push((node.clone(), name.clone(), "input".to_owned()));
    }
    for (unit, node) in &units {
        diagram.push_str(&format!(
            "  subgraph {node}[\"{}\"]\n",
            label(unit.as_str())
        ));
        for id in &plan.units()[unit].steps {
            let Some(node) = nodes.get(id) else {
                continue;
            };
            let step = &plan.steps()[id];
            let status = serde_json::to_value(state.status(id))
                .expect("status serializes")
                .as_str()
                .unwrap()
                .to_owned();
            let doc: String = step
                .doc
                .as_deref()
                .unwrap_or("")
                .replace('\n', " ")
                .chars()
                .take(60)
                .collect();
            let progress = metadata
                .get(id)
                .and_then(|m| m.total.map(|total| format!(" {}/{total}", m.done)))
                .unwrap_or_default();
            let text = format!(
                "{id} / {} / {status}{progress}{}",
                step.run,
                if doc.is_empty() {
                    String::new()
                } else {
                    format!(" / {doc}")
                }
            );
            diagram.push_str(&format!("    {node}[\"{}\"]:::{status}\n", label(&text)));
            let class = if metadata.get(id).is_some_and(|m| m.manual)
                && state.status(id) == StepStatus::Succeeded
            {
                diagram.push_str(&format!("    class {node} manual\n"));
                format!("{status} manual")
            } else {
                status
            };
            boxes.push((node.clone(), text, class));
        }
        diagram.push_str("  end\n");
    }
    let mut edges = vec![];
    let source = |reference: &gates::ValueRef| {
        reference.parts().ok().and_then(|r| {
            if let Some(step) = r.step {
                nodes.get(&step).cloned()
            } else {
                inputs.get(&r.name).cloned()
            }
        })
    };
    for id in visible {
        let target = &nodes[id];
        let step = &plan.steps()[id];
        for reference in step.bindings.values().flat_map(Binding::references) {
            if let Some(from) = source(reference) {
                edges.push((from, target.clone(), reference.to_string(), false));
            }
        }
        for gate in &step.after {
            match gate {
                Gate::Step { id, accept_skip } => {
                    if let Some(from) = nodes.get(id) {
                        edges.push((
                            from.clone(),
                            target.clone(),
                            if *accept_skip {
                                "after ?".into()
                            } else {
                                "after".into()
                            },
                            *accept_skip,
                        ));
                    }
                }
                Gate::Unit { name, accept_skip } => {
                    if let Some(from) = units.get(name) {
                        edges.push((from.clone(), target.clone(), gate.entry(), *accept_skip));
                    }
                }
                Gate::Bool { reference, negate } => {
                    if let Some(from) = source(reference) {
                        edges.push((
                            from,
                            target.clone(),
                            format!("{}{}", if *negate { "not " } else { "" }, reference),
                            false,
                        ));
                    }
                }
            }
        }
    }
    for (i, (name, reference)) in plan.outputs().iter().enumerate() {
        let node = format!("o{i}");
        diagram.push_str(&format!("  {node}([\"{}\"])\n", label(name)));
        if let Some(from) = source(reference) {
            edges.push((from, node.clone(), reference.to_string(), false));
        }
        boxes.push((node, name.clone(), "output".into()));
    }
    for (from, to, text, dashed) in &edges {
        diagram.push_str(&format!(
            "  {from} {}|\"{}\"| {to}\n",
            if *dashed { "-.->" } else { "-->" },
            label(text)
        ));
    }
    diagram.push_str("  classDef pending fill:#e5e7eb,color:#111827\n  classDef running fill:#bfdbfe,color:#111827\n  classDef succeeded fill:#bbf7d0,color:#111827\n  classDef failed fill:#172554,color:#fff,stroke-width:3px\n  classDef stale fill:#fde68a,color:#111827\n  classDef skipped fill:#f3f4f6,color:#6b7280\n  classDef manual fill:#fff,stroke:#16a34a,stroke-width:2px\n");
    let omitted = if done.is_empty() {
        String::new()
    } else {
        format!(
            "{} done units ({} steps) left out",
            done.len(),
            done.iter().map(|unit| unit.steps.len()).sum::<usize>()
        )
    };
    if !omitted.is_empty() {
        diagram.push_str(&format!("  %% {omitted}\n"));
    }
    if matches!(format, PlanViewFormat::Mermaid) {
        return diagram;
    }
    // A self-contained SVG, with the same typed nodes/relations as the text form.
    let mut positions: IndexMap<_, _> = boxes
        .iter()
        .enumerate()
        .map(|(i, (id, _, _))| (id.clone(), i))
        .collect();
    for (name, node) in &units {
        if let Some(position) = plan.units()[name]
            .exits
            .iter()
            .find_map(|id| nodes.get(id).and_then(|node| positions.get(node)).copied())
        {
            positions.insert(node.clone(), position);
        }
    }
    let mut svg = format!(
        "<svg role=\"img\" aria-label=\"Plan graph\" viewBox=\"0 0 1000 {}\" xmlns=\"http://www.w3.org/2000/svg\"><defs><marker id=\"arrow\" viewBox=\"0 0 10 10\" refX=\"10\" refY=\"5\" markerWidth=\"6\" markerHeight=\"6\" orient=\"auto-start-reverse\"><path d=\"M 0 0 L 10 5 L 0 10 z\" fill=\"#64748b\"/></marker></defs>",
        boxes.len().max(1) * 100 + 30
    );
    for (from, to, text, dashed) in edges {
        let a = positions[&from] * 100 + 55;
        let b = positions[&to] * 100 + 55;
        svg.push_str(&format!("<path d=\"M 690 {a} C 800 {a},800 {b},690 {b}\" fill=\"none\" stroke=\"#64748b\" {} marker-end=\"url(#arrow)\"/><text x=\"805\" y=\"{}\" font-size=\"11\">{}</text>", if dashed {"stroke-dasharray=\"4 4\""} else {""}, (a+b)/2,html(&text)));
    }
    for (i, (_, text, status)) in boxes.iter().enumerate() {
        svg.push_str(&format!("<g class=\"{}\"><rect x=\"20\" y=\"{}\" width=\"670\" height=\"70\" rx=\"8\"/><text x=\"35\" y=\"{}\" font-size=\"12\">{}</text></g>",html(status),i*100+20,i*100+58,html(text)));
    }
    svg.push_str("</svg>");
    format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>body{{margin:auto;padding:24px;max-width:1100px;font:16px system-ui;color:#172554;background:#fff}}svg{{width:100%;height:auto}}rect{{fill:#e5e7eb;stroke:#64748b}}.running rect{{fill:#bfdbfe}}.succeeded rect{{fill:#bbf7d0}}.failed rect{{fill:#fecaca;stroke-width:3}}.stale rect{{fill:#fde68a}}.manual rect{{fill:none;stroke:#16a34a;stroke-width:2}}pre{{white-space:pre-wrap;overflow-wrap:anywhere}}@media(prefers-color-scheme:dark){{body{{background:#0f172a;color:#e2e8f0}}text{{fill:#334155}}}}</style><h1>{}</h1><p>{}</p>{svg}<details><summary>Mermaid source</summary><pre>{}</pre></details></html>",
        html(name.as_str()),
        html(name.as_str()),
        html(&omitted),
        html(&diagram)
    )
}
