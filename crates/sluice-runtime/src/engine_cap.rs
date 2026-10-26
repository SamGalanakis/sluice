//! One home-wide queue for engine slots. Count durable unfinished runs, excluding parks;
//! check and take a slot in the same writer transaction as reservation or wake.
use crate::{config::EngineCaps, dispatch::Catalog, plan_cache::PlanCache};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use sluice_model::{
    commands::StepStatus,
    gates::{GateDecision, evaluate_step},
    hash::EffectiveInput,
    ids::*,
    plan::effective_inputs,
    rpc::JsonMap,
};
use sluice_store::{Result, WriteTransaction, plans, resources};
use std::collections::BTreeMap;

pub(crate) fn engine(name: &str, inputs: &JsonMap) -> Option<String> {
    match name {
        "agent.claude" | "agent.review" | "decide.llm" => Some("claude".into()),
        "agent.codex" => Some("codex".into()),
        "agent.devin" => Some("devin".into()),
        _ => inputs
            .0
            .get("engine")
            .and_then(|v| v.as_value().as_str())
            .map(str::to_owned),
    }
}
fn home(sql: &Connection) -> &std::path::Path {
    sql.path()
        .and_then(|p| std::path::Path::new(p).parent())
        .unwrap_or_else(|| std::path::Path::new(""))
}
pub(crate) fn caps(sql: &Connection) -> Result<EngineCaps> {
    crate::config::load_engine_caps(home(sql)).map_err(|e| crate::budget::invalid(e).into())
}
// Fallbacks count runs pinned to releases predating the cap, including native agents nested
// inside a Python fn. The frozen engine wins over later edits to the plan.
const RUN_ENGINE: &str = "coalesce(json_extract(a.request,'$.engine'),r.agent_engine,json_extract(a.request,'$.inputs.engine'),CASE coalesce(json_extract(a.request,'$.declaration.run'),json_extract(a.request,'$.function.name')) WHEN 'agent.codex' THEN 'codex' WHEN 'agent.devin' THEN 'devin' WHEN 'agent.claude' THEN 'claude' WHEN 'agent.review' THEN 'claude' WHEN 'decide.llm' THEN 'claude' END)";

fn holders(sql: &Connection, engine: &str) -> Result<BTreeMap<String, u64>> {
    let mut q = sql.prepare(&format!("SELECT coalesce(p.name,'(home)'),count(*) FROM runs r JOIN attempts a USING(attempt_id) LEFT JOIN projects p ON p.project_id=r.project_id WHERE r.finished_at IS NULL AND {RUN_ENGINE}=?1 AND coalesce(json_extract(a.request,'$.park.phase'),'') NOT IN ('parked','waking') GROUP BY p.project_id"))?;
    let mut out = BTreeMap::new();
    for row in q.query_map([engine], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
    })? {
        let (project, count) = row?;
        *out.entry(project).or_default() += count;
    }
    Ok(out)
}
#[derive(Clone)]
struct Candidate {
    project: Option<ProjectId>,
    step: Option<StepId>,
    run: Option<RunId>,
    engine: String,
    priority: i64,
    since: String,
    count: u64,
    token: Value,
}
fn queue(
    sql: &Connection,
    catalog: &Catalog,
    cache: &PlanCache,
    caps: &EngineCaps,
) -> Result<Vec<Candidate>> {
    let mut out = vec![];
    if !caps.values().any(|c| c.max_runs.is_some()) {
        return Ok(out);
    }
    let policy = crate::budget::policy(home(sql))?;
    let now: String = sql.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
        r.get(0)
    })?;
    let mut q = sql.prepare("SELECT project_id,created_at FROM projects WHERE deleted_at IS NULL AND paused=0 ORDER BY created_at,project_id")?;
    let projects = q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (project, created) in projects {
        let project: ProjectId = crate::calls::parse_id(project)?;
        // A broken registry in one project must not block the home's other projects.
        let context = match crate::coordinator::cached_context(sql, project, catalog, cache) {
            Ok(context) => context,
            Err(_) => continue,
        };
        let state = plans::read_state(sql, project)?;
        for id in context.plan.topological_order() {
            let step = &context.plan.steps()[id];
            let continuing = state.status(id) == StepStatus::Running && step.scatter.is_some();
            if step.is_external()
                || state.paused.is_paused()
                || step.paused.is_paused()
                || (!continuing
                    && (state.status(id) != StepStatus::Pending
                        || !matches!(
                            evaluate_step(&context.plan, &state, step),
                            GateDecision::Ready
                        )))
            {
                continue;
            }
            let needs = step.needs.iter().map(|(n, a)| (n.clone(), *a)).collect();
            if !continuing && !resources::fits(sql, project, &needs)?.fits() {
                continue;
            }
            if crate::budget::resolve(sql, Some(project), step.limits.as_ref(), &policy).is_err() {
                continue;
            }
            let effective = effective_inputs(&context.plan, &state, step).unwrap_or_default();
            let mut inputs = JsonMap(
                effective
                    .into_iter()
                    .filter_map(|(n, v)| match v {
                        EffectiveInput::Value(v) => Some((n, v)),
                        _ => None,
                    })
                    .collect(),
            );
            if continuing {
                let frozen: String = sql.query_row("SELECT a.request FROM attempts a JOIN steps s ON s.project_id=a.project_id AND s.step_id=a.step_id AND s.generation=a.generation AND s.work_generation=a.work_generation WHERE s.project_id=?1 AND s.step_id=?2 ORDER BY a.created_at,a.attempt_id LIMIT 1",(project.to_string(),id.as_str()),|r|r.get(0))?;
                let frozen: Value = serde_json::from_str(&frozen)?;
                inputs = serde_json::from_value(frozen["effective_inputs"].clone())?;
            }
            let (generation,work,stored,total,instances,position,old_hash):(i64,i64,Option<String>,Option<i64>,String,i64,Option<String>) = sql.query_row("SELECT generation,work_generation,engine_queue,total,instances,position,inputs_hash FROM steps WHERE project_id=?1 AND step_id=?2",(project.to_string(),id.as_str()),|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
            let instances: Value = serde_json::from_str(&instances)?;
            let size = step.scatter.as_ref().map_or(1, |n| {
                inputs
                    .0
                    .get(n)
                    .and_then(|v| v.as_value().as_array())
                    .map_or(0, |v| v.len())
            });
            let hash =
                sluice_model::plan::inputs_hash(&context.plan, &state, step).map(|h| h.to_string());
            let keep =
                continuing || old_hash.is_some() && old_hash == hash && total == Some(size as i64);
            let mut engines = BTreeMap::<String, u64>::new();
            let fixed_engine = engine(&step.run, &inputs);
            for index in 0..size {
                if step.scatter.is_some()
                    && keep
                    && instances.get(index.to_string()).is_some_and(|v| {
                        v["status"] == "succeeded"
                            || continuing
                                && matches!(v["status"].as_str(), Some("running" | "failed"))
                    })
                {
                    continue;
                }
                let engine = if step.scatter.as_deref() == Some("engine") {
                    let item = JsonMap(
                        [(
                            "engine".into(),
                            inputs.0["engine"].as_value()[index].clone().try_into()?,
                        )]
                        .into(),
                    );
                    engine(&step.run, &item)
                } else {
                    fixed_engine.clone()
                };
                if let Some(engine) = engine {
                    *engines.entry(engine).or_default() += 1;
                }
            }
            for (engine, count) in engines {
                if caps.get(&engine).and_then(|c| c.max_runs).is_none() {
                    continue;
                }
                let token = json!({"generation":generation,"work":work});
                let stored: Option<Value> =
                    stored.as_deref().map(serde_json::from_str).transpose()?;
                let since = stored
                    .as_ref()
                    .filter(|s| s["token"] == token)
                    .and_then(|s| s["since"].as_str())
                    .unwrap_or(&now)
                    .to_owned();
                out.push(Candidate {
                    project: Some(project),
                    step: Some(id.clone()),
                    run: None,
                    engine,
                    priority: step.priority,
                    since: format!("{since}/{created}/{project}/{position:020}/{id}"),
                    count,
                    token,
                });
            }
        }
    }
    let mut q = sql.prepare(&format!("SELECT r.project_id,r.step_id,r.run_id,{RUN_ENGINE},s.priority,strftime('%Y-%m-%dT%H:%M:%fZ',coalesce(json_extract(a.request,'$.engine_waking_at'),json_extract(a.request,'$.park.parked_at'))) FROM runs r JOIN attempts a USING(attempt_id) JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id JOIN projects p ON p.project_id=r.project_id WHERE r.finished_at IS NULL AND p.deleted_at IS NULL AND json_extract(a.request,'$.park.phase')='waking' AND NOT EXISTS(SELECT 1 FROM leases l WHERE l.run_id=r.run_id AND l.kind='needs' AND l.state='waiting')"))?;
    let wakes = q
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (p, s, r, e, priority, since) in wakes {
        if let Some(engine) = e.filter(|e| caps.get(e).and_then(|c| c.max_runs).is_some()) {
            out.push(Candidate {
                project: Some(crate::calls::parse_id(p)?),
                step: Some(crate::calls::parse_id(s)?),
                run: Some(crate::calls::parse_id(r)?),
                engine,
                priority,
                since,
                count: 1,
                token: Value::Null,
            });
        }
    }
    let mut q = sql.prepare("SELECT c.project_id,c.call_id,c.fn,c.inputs,strftime('%Y-%m-%dT%H:%M:%fZ',c.created_at) FROM calls c LEFT JOIN projects p ON p.project_id=c.project_id WHERE c.status='pending' AND (c.project_id IS NULL OR p.deleted_at IS NULL AND p.paused=0) ORDER BY c.created_at,c.call_id")?;
    let calls = q
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (project, call, name, inputs, since) in calls {
        let project = project.map(crate::calls::parse_id).transpose()?;
        let inputs: JsonMap = serde_json::from_str(&inputs)?;
        let Some(engine) =
            engine(&name, &inputs).filter(|e| caps.get(e).and_then(|c| c.max_runs).is_some())
        else {
            continue;
        };
        if let Ok(function) = crate::calls::CallRegistry::freeze(catalog, project, &name) {
            if let Some(project) = project
                && !resources::fits(sql, project, &function.needs)?.fits()
            {
                continue;
            }
            out.push(Candidate {
                project,
                step: None,
                run: Some(crate::calls::parse_id(call)?),
                engine,
                priority: 0,
                since,
                count: 1,
                token: Value::Null,
            });
        }
    }
    out.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.since.cmp(&b.since))
    });
    Ok(out)
}

pub(crate) fn refresh(
    tx: &mut WriteTransaction<'_>,
    catalog: &Catalog,
    cache: &PlanCache,
) -> Result<()> {
    let caps = caps(tx.sql())?;
    let queue = queue(tx.sql(), catalog, cache, &caps)?;
    // Keep readiness time through coordinator restarts, and reset it when dependencies,
    // pauses or project resources cease to make this a contender.
    let active: std::collections::BTreeSet<_> = queue
        .iter()
        .filter(|c| c.run.is_none())
        .filter_map(|c| Some((c.project?.to_string(), c.step.as_ref()?.to_string())))
        .collect();
    let mut q = tx
        .sql()
        .prepare("SELECT project_id,step_id FROM steps WHERE engine_queue IS NOT NULL")?;
    let stored = q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(q);
    let mut changed = 0;
    for (p, s) in stored {
        if !active.contains(&(p.clone(), s.clone())) {
            changed += tx.sql().execute(
                "UPDATE steps SET engine_queue=NULL WHERE project_id=?1 AND step_id=?2",
                (p, s),
            )?;
        }
    }
    for c in queue.iter().filter(|c| c.run.is_none()) {
        let stamp = json!({"token":c.token,"since":c.since.split('/').next().unwrap_or(&c.since)})
            .to_string();
        changed += tx.sql().execute("UPDATE steps SET engine_queue=?3 WHERE project_id=?1 AND step_id=?2 AND engine_queue IS NOT ?3",(c.project.expect("step project").to_string(),c.step.as_ref().expect("step").as_str(),stamp))?;
    }
    if changed > 0 {
        tx.changed(None, "execution");
    }
    Ok(())
}

pub(crate) fn hold(
    sql: &Connection,
    catalog: &Catalog,
    cache: &PlanCache,
    engine: &str,
    project: ProjectId,
    step: &StepId,
    run: Option<RunId>,
) -> Result<Option<String>> {
    let caps = caps(sql)?;
    let Some(max) = caps.get(engine).and_then(|c| c.max_runs) else {
        return Ok(None);
    };
    let held: u64 = holders(sql, engine)?.values().sum();
    let queue = queue(sql, catalog, cache, &caps)?;
    let ahead: u64 = queue
        .iter()
        .filter(|c| c.engine == engine)
        .take_while(|c| {
            !(c.project == Some(project) && c.step.as_ref() == Some(step) && c.run == run)
        })
        .map(|c| c.count)
        .sum();
    Ok((held >= max || ahead >= max.saturating_sub(held))
        .then(|| format!("held: engine cap ({engine}, {held}/{max} held, {ahead} queued ahead)")))
}

pub(crate) fn shown(sql: &Connection, catalog: &Catalog, cache: &PlanCache) -> Result<Value> {
    let caps = caps(sql)?;
    let queue = queue(sql, catalog, cache, &caps)?;
    let mut out = serde_json::Map::new();
    for (engine, config) in &caps {
        let Some(max) = config.max_runs else {
            continue;
        };
        let projects = holders(sql, engine)?;
        let held: u64 = projects.values().sum();
        let queued: u64 = queue
            .iter()
            .filter(|c| c.engine == *engine)
            .map(|c| c.count)
            .sum();
        out.insert(
            engine.clone(),
            json!({"max_runs":max,"held":held,"projects":projects,"queued":queued}),
        );
    }
    Ok(Value::Object(out))
}

pub(crate) fn run_engine(sql: &Connection, run: RunId) -> Result<Option<String>> {
    Ok(sql.query_row(&format!("SELECT {RUN_ENGINE} FROM runs r JOIN attempts a USING(attempt_id) WHERE r.run_id=?1"),[run.to_string()],|r|r.get(0)).optional()?.flatten())
}

/// Calls share the same held total. Direct calls return the same retryable hold used for
/// memory; queued calls remain pending until the scheduler can reserve them.
pub(crate) fn hold_call(
    sql: &Connection,
    name: &str,
    inputs: &JsonMap,
    call: RunId,
    catalog: Option<&Catalog>,
) -> Result<Option<String>> {
    let Some(engine) = engine(name, inputs) else {
        return Ok(None);
    };
    let caps = caps(sql)?;
    let Some(max) = caps.get(&engine).and_then(|c| c.max_runs) else {
        return Ok(None);
    };
    let held: u64 = holders(sql, &engine)?.values().sum();
    let ahead = if let Some(catalog) = catalog {
        queue(sql, catalog, &PlanCache::default(), &caps)?
            .iter()
            .filter(|c| c.engine == engine)
            .take_while(|c| c.step.is_some() || c.run != Some(call))
            .map(|c| c.count)
            .sum()
    } else {
        0
    };
    Ok((held >= max || ahead >= max.saturating_sub(held))
        .then(|| format!("held: engine cap ({engine}, {held}/{max} held, {ahead} queued ahead)")))
}

/// Status and queue records use one home-wide snapshot, including continuing scatters and
/// waking parks whose stored step status is still running.
pub(crate) fn holds(
    sql: &Connection,
    catalog: &Catalog,
    cache: &PlanCache,
    project: ProjectId,
) -> Result<BTreeMap<StepId, (Vec<String>, String)>> {
    let caps = caps(sql)?;
    let queue = queue(sql, catalog, cache, &caps)?;
    let mut out = BTreeMap::new();
    for (engine, config) in &caps {
        let Some(max) = config.max_runs else {
            continue;
        };
        let held: u64 = holders(sql, engine)?.values().sum();
        let mut ahead = 0;
        for c in queue.iter().filter(|c| c.engine == *engine) {
            if c.project == Some(project)
                && let Some(step) = &c.step
                && (held >= max || ahead >= max.saturating_sub(held))
            {
                out.entry(step.clone()).or_insert_with(|| {
                    (
                        vec![format!("engine:{engine}")],
                        format!(
                            "held: engine cap ({engine}, {held}/{max} held, {ahead} queued ahead)"
                        ),
                    )
                });
            }
            ahead += c.count;
        }
    }
    Ok(out)
}
