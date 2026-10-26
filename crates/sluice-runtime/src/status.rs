//! `status` (SPEC §8): the steps view, or the units view's compact rows, from one read
//! snapshot. The unit logic is sluice_model::status; this gathers the store's facts.
use crate::dispatch::Catalog;
use rusqlite::Connection;
use serde_json::{Value, json};
use sluice_model::{
    commands::{StatusQuery, StatusView, StepStatus},
    error::PublicError,
    gates::{resolve_reference, wait_reasons},
    ids::{ProjectId, Revision, StepId, UnitName},
    plan::Pause,
    status::{self, LastMessage, StepFacts},
    types::BoundValue,
};
use sluice_store::{plans, resources};
use std::collections::BTreeMap;

/// A pending `core.external` step with nothing else holding it: sluice never starts it.
const EXTERNAL_WAIT: &str = "external: set its outputs with step_set_output";

fn bad(message: impl Into<String>) -> sluice_store::StoreError {
    PublicError::BadRequest {
        message: message.into(),
    }
    .into()
}
fn step_id(raw: String) -> sluice_store::Result<StepId> {
    raw.parse()
        .map_err(|e| sluice_store::StoreError::InvalidDatabase(format!("{e}")))
}

/// Each ready step short of resources, with its `queued: …` reason.
fn queued(
    sql: &Connection,
    id: ProjectId,
    rev: Revision,
    plan: &sluice_model::Plan,
    engine_context: Option<(&Catalog, &crate::plan_cache::PlanCache)>,
) -> sluice_store::Result<BTreeMap<StepId, (Vec<String>, String)>> {
    let mut out = BTreeMap::new();
    for candidate in resources::admit_order(sql, id, rev, plan)? {
        let fit = resources::fits(sql, id, &candidate.needs)?;
        if !fit.fits() {
            out.insert(
                candidate.step,
                (fit.blocked, format!("queued: {}", fit.reason)),
            );
        }
    }
    let home = sql
        .path()
        .and_then(|p| std::path::Path::new(p).parent())
        .unwrap_or_else(|| std::path::Path::new(""));
    let execution = crate::budget::execution(home);
    // Admission's own last reading of the host, so status says what the scheduler decides.
    let sample = crate::headroom::published(home);
    if execution.is_ok()
        && let Some((catalog, cache)) = engine_context
    {
        for (step, reason) in crate::engine_cap::holds(sql, catalog, cache, id)? {
            out.entry(step).or_insert(reason);
        }
    }
    let state = plans::read_state(sql, id)?;
    for step in plan.steps().values() {
        if step.is_external()
            || state.status(&step.id) != StepStatus::Pending
            || !matches!(
                sluice_model::gates::evaluate_step(plan, &state, step),
                sluice_model::gates::GateDecision::Ready
            )
        {
            continue;
        }
        // A step that would start is held only by admission's measured checks.
        let held = match &execution {
            Ok((policy, headroom)) => {
                crate::budget::resolve(sql, Some(id), step.limits.as_ref(), policy).and_then(|_| {
                    sample
                        .as_ref()
                        .map(|reading| {
                            Ok(crate::headroom::hold(
                                &reading.host,
                                headroom,
                                policy.total.memory_bytes,
                                crate::headroom::pool(sql, reading, headroom)?,
                            ))
                        })
                        .transpose()
                        .map(Option::flatten)
                })
            }
            Err(error) => Err(error.clone().into()),
        };
        match held {
            Ok(Some(hold)) => {
                out.entry(step.id.clone())
                    .or_insert((vec![hold.resource().into()], hold.into_reason()));
            }
            // An invalid config.json holds every step; it says so rather than queue them.
            Err(sluice_store::StoreError::Public(error)) if execution.is_err() => {
                out.entry(step.id.clone()).or_insert((
                    vec!["execution".into()],
                    format!("held: invalid execution config ({error})"),
                ));
            }
            Err(error) => {
                out.entry(step.id.clone())
                    .or_insert((vec!["execution".into()], format!("queued: {error}")));
            }
            _ => {}
        }
    }
    Ok(out)
}

fn resources_value(
    sql: &Connection,
    id: ProjectId,
    rev: Revision,
    plan: &sluice_model::Plan,
) -> sluice_store::Result<BTreeMap<String, Value>> {
    Ok(resources::status(sql, id, rev, plan)?
        .into_iter()
        .map(|(n, r)| {
            (
                n,
                json!({"capacity":r.resource.capacity,"held":r.held,"queued":r.queued,"error":r.resource.error}),
            )
        })
        .collect())
}

pub(crate) fn status(
    sql: &Connection,
    catalog: &Catalog,
    cache: &crate::plan_cache::PlanCache,
    query: StatusQuery,
) -> sluice_store::Result<Value> {
    if query.view == StatusView::Units && query.brief {
        return Err(bad("brief: the units view has no values to cut"));
    }
    if query.view == StatusView::Steps && query.state.is_some() {
        return Err(bad(
            "state: only the units view (view: \"units\") filters by state",
        ));
    }
    let id = sluice_store::messages::resolve_project(sql, &query.project)?;
    let ctx = crate::coordinator::cached_context(sql, id, catalog, cache)?;
    let plan = &ctx.plan;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let state = plans::read_state(sql, id)?;
    let selected = status::select(
        plan,
        query.selection.steps.as_deref(),
        query.selection.tags.as_deref(),
    )
    .map_err(|message| sluice_store::StoreError::from(PublicError::NotFound { message }))?;
    let (project, board_rev) = {
        let p = sluice_store::projects::resolve(sql, &sluice_model::ids::ProjectSelector::Id(id))?;
        (json!({"project_id":id,"name":p.name}), p.board_rev)
    };
    if query.view == StatusView::Running {
        if query.brief || query.state.is_some() {
            return Err(bad(
                "brief and state apply to the steps and units views respectively",
            ));
        }
        return running(sql, id, plan, &state, selected.as_ref());
    }
    let queued = queued(sql, id, ctx.revision, plan, Some((catalog, cache)))?;
    let parks = resources::parked_steps(sql, id)?;
    let resources = resources_value(sql, id, ctx.revision, plan)?;
    let paused = state.paused.is_paused();
    let mut finishing = sluice_store::attempts::finishing(sql, id)?;
    // each step's and unit's title (SPEC §13), derived from the plan, its recipes and prompts
    let home = sql
        .path()
        .and_then(|p| std::path::Path::new(p).parent())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    let names = crate::naming::for_project(sql, &home, id)?;
    let execution_settings =
        crate::config::load_execution_with_total(&home).map_err(crate::budget::invalid);
    let execution_policy = execution_settings
        .as_ref()
        .map(|(policy, _, _)| policy.clone())
        .map_err(Clone::clone);
    let execution = match &execution_settings {
        Ok((policy, headroom, total)) => json!({
            "total": total,
            "memory": crate::headroom::shown(sql, &home, policy, headroom)?,
            "cpu": crate::headroom::shown_cpu(sql, &home, headroom)?,
            "engines": crate::engine_cap::shown(sql, catalog, cache)?,
        }),
        Err(error) => json!({"error":error}),
    };
    if query.view == StatusView::Units {
        let facts = facts(sql, id, &queued, finishing)?;
        let last = last_messages(sql, id)?;
        let mut view = status::units_view(
            plan,
            &state,
            &facts,
            &last,
            selected.as_ref(),
            query.all,
            query.state.as_deref(),
        );
        // Tool summaries retain a replyable id; the dashboard's unit_rows is separate.
        for row in &mut view.rows {
            let name = row
                .unit
                .parse::<UnitName>()
                .map_err(|e| sluice_store::StoreError::InvalidDatabase(e.to_string()))?;
            if let Some(unit) = plan.units().get(&name) {
                let reasons: Vec<_> = unit
                    .steps
                    .iter()
                    .filter_map(|id| queued.get(id).map(|(_, reason)| reason.as_str()))
                    .filter(|reason| !row.blocked.contains(reason))
                    .collect();
                if !reasons.is_empty() {
                    if !row.blocked.is_empty() {
                        row.blocked.push_str("; ");
                    }
                    row.blocked.push_str(&reasons.join("; "));
                    row.line = status::line(row);
                }
            }
            if let Some(message) = plan.units().get(&name).and_then(|unit| {
                unit.steps
                    .iter()
                    .filter_map(|id| last.get(id))
                    .max_by_key(|m| m.id)
            }) {
                row.last = format!("#{} {}", message.id, row.last);
                row.line = status::line(row);
            }
        }
        let mut out =
            json!({"project":project,"rev":ctx.revision,"board_rev":board_rev,"paused":paused});
        if !resources.is_empty() {
            out["resources"] = json!(resources);
        }
        let mut rows = serde_json::to_value(&view.rows)?;
        for row in rows.as_array_mut().into_iter().flatten() {
            let unit = row["unit"].as_str().unwrap_or_default().to_owned();
            let restarts = plan
                .units()
                .get(
                    &unit
                        .parse::<UnitName>()
                        .map_err(|e| sluice_store::StoreError::InvalidDatabase(e.to_string()))?,
                )
                .map(|u| {
                    u.steps
                        .iter()
                        .map(|step| {
                            Ok((
                                step.to_string(),
                                sluice_store::restart::status(sql, &ctx, step, now)?,
                            ))
                        })
                        .collect::<sluice_store::Result<BTreeMap<_, _>>>()
                })
                .transpose()?
                .unwrap_or_default();
            row["restart"] = json!(
                restarts
                    .into_iter()
                    .filter_map(|(step, status)| status.map(|s| (step, s)))
                    .collect::<BTreeMap<_, _>>()
            );
            let steps: Vec<_> = plan
                .units()
                .get(
                    &unit
                        .parse::<UnitName>()
                        .map_err(|e| sluice_store::StoreError::InvalidDatabase(e.to_string()))?,
                )
                .map(|u| u.steps.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            let mut timings = serde_json::Map::new();
            let mut due = vec![];
            let mut classes = serde_json::Map::new();
            for step in steps {
                if let Some(timing) = crate::run_timing::latest(sql, id, &step)? {
                    let mut value = serde_json::to_value(timing)?;
                    value.as_object_mut().expect("timing object").remove("time");
                    timings.insert(step.clone(), value);
                }
                due.extend(crate::run_health::due(sql, id, &step, now)?);
                let raw: Option<String> = sql.query_row(
                    "SELECT error FROM steps WHERE project_id=?1 AND step_id=?2",
                    (id.to_string(), &step),
                    |r| r.get(0),
                )?;
                if let Some(raw) = raw {
                    let error: PublicError = serde_json::from_str(&raw)?;
                    classes.insert(step, json!(error.failure_class()));
                }
            }
            row["timing"] = json!(timings);
            row["check_due"] = json!(due);
            row["failure_class"] = json!(classes);
            let named = names.naming.unit(&unit);
            row["title"] = json!(names.naming.unit_title(&unit));
            if let Some(recipe) = named.map(|n| n.recipe.as_str()).filter(|r| !r.is_empty()) {
                row["recipe"] = json!(recipe);
            }
        }
        out["units"] = rows;
        out["execution"] = json!(execution);
        if let Some((units, steps)) = view.done {
            out["done_units"] = json!({"units":units,"steps":steps});
        }
        return Ok(out);
    }
    let mut q = sql.prepare("SELECT step_id,json_object('status',status,'outputs',json(outputs),'error',json(error),'run_ids',json(run_ids),'done',done,'total',total,'instances',json(instances),'manual',manual) FROM steps WHERE project_id=?1 ORDER BY position")?;
    let rows = q
        .query_map([id.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let done = match (&selected, query.all) {
        (None, false) => status::done_units(plan, &state),
        _ => vec![],
    };
    let left_out: std::collections::HashSet<&StepId> =
        done.iter().flat_map(|u| u.steps.iter()).collect();
    let presets = sluice_store::bypass::presets(sql, id)?;
    let mut steps = serde_json::Map::new();
    for (step, row) in rows {
        let step = step_id(step)?;
        let shown = match &selected {
            Some(selected) => selected.contains(&step),
            None => !left_out.contains(&step),
        };
        if !shown {
            continue;
        }
        let mut row: Value = serde_json::from_str(&row)?;
        crate::run_timing::add(&mut row, crate::run_timing::latest(sql, id, step.as_str())?)?;
        sluice_model::error::show_failure(&mut row);
        row["check_due"] = json!(crate::run_health::due(sql, id, step.as_str(), now)?);
        if let Some(park) = parks.get(&step) {
            row["shown"] = json!("parked");
            row["parked"] = json!(park);
        }
        if let Some(restart) = sluice_store::restart::status(sql, &ctx, &step, now)? {
            row["restart"] = json!(restart.policy);
            row["restart_count"] = json!(restart.count);
            if let Some(at) = restart.next_at {
                row["restart_at"] = json!(at);
            }
        }
        if query.brief
            && let Some(outputs) = row.get_mut("outputs")
        {
            *outputs = status::brief(outputs);
        }
        if let (Some(spec), Value::Object(row)) = (plan.steps().get(&step), &mut row) {
            if !spec.is_external() {
                let runs = row
                    .get("run_ids")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut evidence = Vec::new();
                for run in runs {
                    if let Some(run) = run.as_str() {
                        let run = run.parse().map_err(|e| {
                            sluice_store::StoreError::InvalidDatabase(format!("{e}"))
                        })?;
                        evidence.push(crate::budget::run_resources(sql, run)?);
                    }
                }
                if !evidence.is_empty() {
                    row.insert("run_resources".into(), json!(evidence));
                }
                if state.status(&step) == StepStatus::Pending {
                    match &execution_policy {
                        Ok(policy) => match crate::budget::resolve(
                            sql,
                            Some(id),
                            spec.limits.as_ref(),
                            policy,
                        ) {
                            Ok(limits) => {
                                row.insert("limits".into(), limits.shown());
                            }
                            Err(error) => {
                                row.insert("budget_error".into(), json!(error.to_string()));
                            }
                        },
                        Err(error) => {
                            row.insert("budget_error".into(), json!(error));
                        }
                    }
                } else if let Some(limits) = evidence.last().and_then(|e| e.get("limits")) {
                    row.insert("limits".into(), limits.clone());
                }
            }
            row.insert(
                "title".into(),
                json!(names.naming.step_title(step.as_str())),
            );
            if let Some(stage) = names
                .naming
                .step(step.as_str())
                .map(|n| n.stage.as_str())
                .filter(|s| !s.is_empty())
            {
                row.insert("stage".into(), json!(stage));
            }
            match &spec.paused {
                Pause::No => {}
                Pause::Yes => {
                    row.insert("paused".into(), json!(true));
                }
                Pause::Reason(reason) => {
                    row.insert("paused".into(), json!(reason));
                }
            }
            if let Some(finishing) = finishing.remove(&step) {
                row.insert("finishing".into(), serde_json::to_value(finishing)?);
            }
            if state.status(&step) == StepStatus::Pending || queued.contains_key(&step) {
                let mut waiting = wait_reasons(plan, &state, spec);
                if let Some((blocked, reason)) = queued.get(&step) {
                    row.insert("queued".into(), json!(blocked));
                    waiting.push(reason.clone());
                }
                if waiting.is_empty() && spec.is_external() {
                    waiting.push(EXTERNAL_WAIT.into());
                }
                if !waiting.is_empty() {
                    row.insert("waiting".into(), json!(waiting));
                }
                if let Some(preset) = presets.get(&step).filter(|p| p.pending()) {
                    row.insert("bypass".into(), preset.summary());
                }
            }
        }
        steps.insert(step.to_string(), row);
    }
    let outputs: serde_json::Map<String, Value> = plan
        .outputs()
        .iter()
        .filter_map(|(n, r)| match resolve_reference(plan, &state, r) {
            BoundValue::Ready(v) => Some((n.clone(), v.as_value().clone())),
            _ => None,
        })
        .collect();
    let mut inputs = serde_json::to_value(&state.inputs)?;
    let mut outputs = Value::Object(outputs);
    if query.brief {
        inputs = status::brief(&inputs);
        outputs = status::brief(&outputs);
    }
    let mut out = json!({"project":project,"rev":ctx.revision,"board_rev":board_rev,"paused":paused,"inputs":inputs,"steps":steps,"outputs":outputs,"resources":resources,"execution":execution});
    if !done.is_empty() {
        out["done_units"] = json!({"units":done.len(),"steps":left_out.len()});
    }
    Ok(out)
}

/// The units view's rows for a plan already compiled at `rev`, as `status(view: "units")`
/// builds them: what a board's `Units` draws. `wanted` keeps the units in those states; done
/// units are left out (and counted) unless `wanted` names `settled`.
pub fn unit_rows(
    sql: &Connection,
    id: ProjectId,
    rev: Revision,
    plan: &sluice_model::Plan,
    wanted: Option<&[sluice_model::commands::UnitState]>,
) -> sluice_store::Result<status::UnitsView> {
    let state = plans::read_state(sql, id)?;
    let queued = queued(sql, id, rev, plan, None)?;
    let finishing = sluice_store::attempts::finishing(sql, id)?;
    let facts = facts(sql, id, &queued, finishing)?;
    let last = last_messages(sql, id)?;
    let all = wanted.is_some_and(|w| w.contains(&sluice_model::commands::UnitState::Settled));
    Ok(status::units_view(
        plan, &state, &facts, &last, None, all, wanted,
    ))
}

/// Seconds-ago figures come from SQLite's clock over the stored RFC 3339 times.
fn facts(
    sql: &Connection,
    id: ProjectId,
    queued: &BTreeMap<StepId, (Vec<String>, String)>,
    finishing: BTreeMap<StepId, sluice_model::attempt::Finishing>,
) -> sluice_store::Result<indexmap::IndexMap<StepId, StepFacts>> {
    let mut facts = indexmap::IndexMap::<StepId, StepFacts>::new();
    for (step, (_, reason)) in queued {
        facts.entry(step.clone()).or_default().queued = Some(reason.clone());
    }
    for (step, finishing) in finishing {
        facts.entry(step).or_default().finishing = Some(finishing);
    }
    for step in sluice_store::attempts::stopping(sql, id)? {
        facts.entry(step).or_default().stopping = true;
    }
    for (step, park) in resources::parked_steps(sql, id)? {
        facts.entry(step).or_default().parked = Some(park);
    }
    // each query's third column is the instant its seconds count from (SQLite takes a bare
    // column from the row its min or max picked)
    let ago = |sql: &Connection, query: &str| -> sluice_store::Result<Vec<(StepId, f64, String)>> {
        let mut q = sql.prepare(query)?;
        let rows = q
            .query_map([id.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<f64>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .filter_map(|(step, secs, at)| Some((step, secs?, at.unwrap_or_default())))
            .map(|(step, secs, at)| Ok((step_id(step)?, secs.max(0.0), at)))
            .collect()
    };
    for (step, secs, at) in ago(
        sql,
        "SELECT r.step_id,max((julianday('now')-julianday(coalesce(r.started_at,r.created_at)))*86400),coalesce(r.started_at,r.created_at)
         FROM runs r JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id
         WHERE r.project_id=?1 AND s.status='running' AND r.finished_at IS NULL GROUP BY r.step_id",
    )? {
        let entry = facts.entry(step).or_default();
        entry.running_for = Some(secs);
        entry.running_since = Some(at);
    }
    let changes = [
        "SELECT s.step_id,(julianday('now')-julianday((SELECT max(at) FROM records r WHERE r.project_id=s.project_id AND r.step_id=s.step_id AND r.kind='step.status')))*86400,(SELECT max(at) FROM records r WHERE r.project_id=s.project_id AND r.step_id=s.step_id AND r.kind='step.status') FROM steps s WHERE s.project_id=?1",
        "SELECT s.step_id,(julianday('now')-julianday((SELECT max(coalesce(finished_at,started_at,created_at)) FROM runs r WHERE r.project_id=s.project_id AND r.step_id=s.step_id)))*86400,(SELECT max(coalesce(finished_at,started_at,created_at)) FROM runs r WHERE r.project_id=s.project_id AND r.step_id=s.step_id) FROM steps s WHERE s.project_id=?1",
    ];
    for query in changes {
        for (step, secs, at) in ago(sql, query)? {
            let entry = facts.entry(step).or_default();
            if entry.changed_ago.is_none_or(|old| secs < old) {
                entry.changed_ago = Some(secs);
                entry.changed_at = Some(at);
            }
        }
    }
    Ok(facts)
}

/// The last message on each step's thread (`step-<id>`).
fn last_messages(
    sql: &Connection,
    id: ProjectId,
) -> sluice_store::Result<indexmap::IndexMap<StepId, LastMessage>> {
    let mut q = sql.prepare(
        "SELECT thread,id,body,needs_reply FROM messages WHERE id IN (
           SELECT (SELECT max(id) FROM messages m WHERE m.project_id=s.project_id AND m.thread='step-'||s.step_id) FROM steps s WHERE s.project_id=?1)",
    )?;
    let rows = q
        .query_map([id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(thread, id, body, needs_reply)| {
            let step = thread.strip_prefix("step-")?.parse::<StepId>().ok()?;
            Some((
                step,
                LastMessage {
                    id,
                    body,
                    needs_reply,
                },
            ))
        })
        .collect())
}

/// Column names once, positional rows: a large project's live work fits one tool reply.
fn running(
    sql: &Connection,
    project: ProjectId,
    plan: &sluice_model::Plan,
    state: &sluice_model::gates::StateSnapshot,
    selected: Option<&indexmap::IndexSet<StepId>>,
) -> sluice_store::Result<Value> {
    let parks = resources::parked_steps(sql, project)?;
    let mut rows = vec![];
    let now = crate::run_health::now_seconds();
    for (id, step) in plan.steps() {
        if state.status(id) != StepStatus::Running || selected.is_some_and(|s| !s.contains(id)) {
            continue;
        }
        let Some(run) = crate::run_timing::run(sql, project, id.as_str(), true)? else {
            continue;
        };
        let unit = step.unit_name();
        let engine = run
            .agent
            .as_ref()
            .map(|a| format!("{} {}", a.engine, a.model.as_deref().unwrap_or("unknown")))
            .unwrap_or_else(|| step.run.to_string());
        let activity: Option<i64> = run
            .timing
            .last_activity_at
            .as_ref()
            .map(|at| sql.query_row("SELECT max(0,?1-unixepoch(?2))", (now, at), |r| r.get(0)))
            .transpose()?;
        let waiting = sql
            .prepare(
                "SELECT resource FROM leases WHERE run_id=?1 AND state='waiting' ORDER BY resource",
            )?
            .query_map([&run.id], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let reason = parks
            .get(id)
            .map(|p| format!("parked {}", p.until))
            .unwrap_or_else(|| {
                if waiting.is_empty() {
                    String::new()
                } else {
                    format!("held {}", waiting.join(","))
                }
            });
        let words = run
            .agent
            .as_ref()
            .map(|a| {
                a.last_message
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let words: String = sluice_agents::engines::account::redact(&words)
            .chars()
            .take(120)
            .collect();
        rows.push(json!([
            unit,
            id,
            engine,
            run.timing.elapsed_s.round(),
            activity,
            reason,
            words,
            run.time.map(|t| t.compact())
        ]));
    }
    // Spend at most ~6 KB on 25 live rows. Identities and timings stay intact; the last
    // words use the remaining space, up to 120 characters per worker.
    let mut out = json!({"columns":["unit","step","engine","elapsed_s","last_activity_age_s","reason","last","time"],"running":rows});
    while out.to_string().len() > 6100 {
        let Some(rows) = out["running"].as_array_mut() else {
            break;
        };
        let longest = rows
            .iter_mut()
            .filter(|r| r[6].as_str().is_some_and(|s| !s.is_empty()))
            .max_by_key(|r| r[6].as_str().map_or(0, str::len));
        let Some(row) = longest else {
            break;
        };
        let text = row[6].as_str().unwrap_or_default();
        row[6] = json!(
            text.chars()
                .take(text.chars().count().saturating_sub(8))
                .collect::<String>()
        );
    }
    Ok(out)
}
