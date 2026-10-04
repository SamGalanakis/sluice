//! `status` (SPEC §8): the steps view, or the units view's compact rows, from one read
//! snapshot. The unit logic is sluice_model::status; this gathers the store's facts.
use crate::dispatch::Catalog;
use rusqlite::Connection;
use serde_json::{Value, json};
use sluice_model::{
    commands::{StatusQuery, StatusView},
    error::PublicError,
    gates::resolve_reference,
    ids::{ProjectId, StepId},
    status::{self, LastMessage, StepFacts},
    types::BoundValue,
};
use sluice_store::{plans, resources};
use std::collections::BTreeMap;

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
    plan: &sluice_model::Plan,
) -> sluice_store::Result<BTreeMap<StepId, (Vec<String>, String)>> {
    let mut out = BTreeMap::new();
    for candidate in resources::admit_order(sql, id, plan)? {
        let fit = resources::fits(sql, id, &candidate.needs)?;
        if !fit.fits() {
            out.insert(
                candidate.step,
                (fit.blocked, format!("queued: {}", fit.reason)),
            );
        }
    }
    Ok(out)
}

fn resources_value(
    sql: &Connection,
    id: ProjectId,
    plan: &sluice_model::Plan,
) -> sluice_store::Result<BTreeMap<String, Value>> {
    Ok(resources::status(sql, id, plan)?
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
    let ctx = crate::coordinator::context(sql, id, catalog)?;
    let plan = &ctx.plan;
    let state = plans::read_state(sql, id)?;
    let selected = status::select(
        plan,
        query.selection.steps.as_deref(),
        query.selection.tags.as_deref(),
    )
    .map_err(|message| sluice_store::StoreError::from(PublicError::NotFound { message }))?;
    let project = {
        let p = sluice_store::projects::resolve(sql, &sluice_model::ids::ProjectSelector::Id(id))?;
        json!({"project_id":id,"name":p.name})
    };
    let queued = queued(sql, id, plan)?;
    let resources = resources_value(sql, id, plan)?;
    let paused = state.paused.is_paused();
    if query.view == StatusView::Units {
        let facts = facts(sql, id, &queued)?;
        let last = last_messages(sql, id)?;
        let view = status::units_view(
            plan,
            &state,
            &facts,
            &last,
            selected.as_ref(),
            query.all,
            query.state.as_deref(),
        );
        let mut out = json!({"project":project,"rev":ctx.revision,"paused":paused});
        if !resources.is_empty() {
            out["resources"] = json!(resources);
        }
        out["units"] = serde_json::to_value(&view.rows)?;
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
        if query.brief
            && let Some(outputs) = row.get_mut("outputs")
        {
            *outputs = status::brief(outputs);
        }
        if let Some((blocked, reason)) = queued.get(&step)
            && let Value::Object(row) = &mut row
        {
            row.insert("queued".into(), json!(blocked));
            row.insert("waiting".into(), json!([reason]));
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
    let mut out = json!({"project":project,"rev":ctx.revision,"paused":paused,"inputs":inputs,"steps":steps,"outputs":outputs,"resources":resources});
    if !done.is_empty() {
        out["done_units"] = json!({"units":done.len(),"steps":left_out.len()});
    }
    Ok(out)
}

/// Seconds-ago figures come from SQLite's clock over the stored RFC 3339 times.
fn facts(
    sql: &Connection,
    id: ProjectId,
    queued: &BTreeMap<StepId, (Vec<String>, String)>,
) -> sluice_store::Result<indexmap::IndexMap<StepId, StepFacts>> {
    let mut facts = indexmap::IndexMap::<StepId, StepFacts>::new();
    for (step, (_, reason)) in queued {
        facts.entry(step.clone()).or_default().queued = Some(reason.clone());
    }
    let ago = |sql: &Connection, query: &str| -> sluice_store::Result<Vec<(StepId, i64)>> {
        let mut q = sql.prepare(query)?;
        let rows = q
            .query_map([id.to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .filter_map(|(step, secs)| secs.map(|s| (step, s)))
            .map(|(step, secs)| Ok((step_id(step)?, secs.max(0.0) as i64)))
            .collect()
    };
    for (step, secs) in ago(
        sql,
        "SELECT r.step_id,max((julianday('now')-julianday(coalesce(r.started_at,r.created_at)))*86400)
         FROM runs r JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id
         WHERE r.project_id=?1 AND s.status='running' AND r.finished_at IS NULL GROUP BY r.step_id",
    )? {
        facts.entry(step).or_default().running_for = Some(secs);
    }
    let changes = [
        "SELECT step_id,min((julianday('now')-julianday(at))*86400) FROM records
         WHERE project_id=?1 AND kind='step.status' AND step_id IS NOT NULL GROUP BY step_id",
        "SELECT step_id,min((julianday('now')-julianday(coalesce(finished_at,started_at,created_at)))*86400)
         FROM runs WHERE project_id=?1 AND step_id IS NOT NULL GROUP BY step_id",
    ];
    for query in changes {
        for (step, secs) in ago(sql, query)? {
            let entry = &mut facts.entry(step).or_default().changed_ago;
            *entry = Some(entry.map_or(secs, |old| old.min(secs)));
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
           SELECT max(id) FROM messages WHERE project_id=?1 AND thread LIKE 'step-%' GROUP BY thread)",
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
