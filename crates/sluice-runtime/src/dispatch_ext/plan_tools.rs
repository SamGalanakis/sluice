use super::plan_values::*;
use crate::{
    calls::public, coordinator::Coordinator, dispatch::Catalog, execution::ExecutionHost, naming,
    plan_cache::PlanCache,
};
use rusqlite::Connection;
use sluice_model::{commands::*, error::PublicError, ids::*, plan_rows::*};
use sluice_store::{messages, plans, projects};
use std::{collections::BTreeMap, path::Path};

fn identity(sql: &Connection, project: ProjectId) -> sluice_store::Result<ProjectIdentity> {
    Ok(ProjectIdentity {
        project_id: project,
        name: projects::resolve(sql, &ProjectSelector::Id(project))?.name,
    })
}
fn recipes(
    sql: &Connection,
    home: &Path,
    project: ProjectId,
) -> sluice_store::Result<BTreeMap<String, String>> {
    Ok((*naming::unit_recipes(sql, home, project)?).clone())
}
fn views(
    sql: &Connection,
    project: ProjectId,
    rows: Vec<StepRowView>,
    compact: bool,
    recipes: &BTreeMap<String, String>,
) -> sluice_store::Result<Vec<StepView>> {
    let references = if compact {
        ReferenceRows(vec![])
    } else {
        plans::read_references(
            sql,
            project,
            &ReferenceSelection::Consumers(rows.iter().map(|r| r.step.clone()).collect()),
        )?
    };
    step_views(rows, references, recipes).map_err(Into::into)
}
fn limit(value: u32) -> Result<u32, PublicError> {
    if value == 0 {
        return Err(PlanRowsError::Limit.into());
    }
    Ok(value.min(MAX_LIMIT))
}

pub async fn dispatch<H: ExecutionHost>(
    broker: &Coordinator<H>,
    request: CommandRequest,
) -> Result<Option<CommandReply>, PublicError> {
    if !matches!(
        request,
        CommandRequest::PlanRead(_)
            | CommandRequest::StepGet(_)
            | CommandRequest::UnitGet(_)
            | CommandRequest::PlanHistory(_)
            | CommandRequest::PlanView(_)
    ) {
        return Ok(None);
    }
    let home = broker.home().to_owned();
    let (catalog, cache) = (broker.catalog().clone(), broker.plan_cache());
    let reply = broker
        .reads()
        .snapshot(move |sql| match request {
            CommandRequest::PlanRead(request) => {
                let page_size = limit(request.limit)?;
                let id = messages::resolve_project(sql, &request.project)?;
                let header = plans::plan_header(sql, id)?;
                let generation = crate::plan_cache::recipe_generation(&home, id);
                let recipes = recipes(sql, &home, id)?;
                let mut selected = row_selection(&request.filter(), &recipes);
                selected.after =
                    cursor_after(&request, id, header.rev, header.state_epoch, &generation)?;
                selected.limit = Some(page_size);
                let rows = plans::read_steps(sql, id, &selected, projection(request.compact))?;
                let next_cursor = next_cursor(&request, id, &rows, &generation);
                Ok(CommandReply::PlanRead(PlanReadResult {
                    project: identity(sql, id)?,
                    rev: rows.rev,
                    state_epoch: rows.state_epoch,
                    recipe_generation: generation,
                    next_cursor,
                    steps: views(sql, id, rows.steps, request.compact, &recipes)?,
                }))
            }
            CommandRequest::StepGet(request) => {
                let id = messages::resolve_project(sql, &request.project)?;
                let recipes = recipes(sql, &home, id)?;
                let selected = RowSelection {
                    steps: Some(vec![request.step.clone()]),
                    ..Default::default()
                };
                let rows = plans::read_steps(sql, id, &selected, projection(request.compact))?;
                let step = views(sql, id, rows.steps, request.compact, &recipes)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        PublicError::from(PlanRowsError::NoStep { step: request.step })
                    })?;
                Ok(CommandReply::Step(StepGetResult {
                    project: identity(sql, id)?,
                    rev: rows.rev,
                    state_epoch: rows.state_epoch,
                    step,
                }))
            }
            CommandRequest::UnitGet(request) => {
                let id = messages::resolve_project(sql, &request.project)?;
                let recipes = recipes(sql, &home, id)?;
                let selected = RowSelection {
                    units: Some(vec![request.unit.clone()]),
                    ..Default::default()
                };
                let rows = plans::read_steps(sql, id, &selected, projection(request.compact))?;
                if rows.steps.is_empty() {
                    return Err(
                        PublicError::from(PlanRowsError::NoUnit { unit: request.unit }).into(),
                    );
                }
                let summary = unit_summary(sql, id, &request.unit, &catalog, &cache)?;
                Ok(CommandReply::Unit(UnitGetResult {
                    project: identity(sql, id)?,
                    rev: rows.rev,
                    state_epoch: rows.state_epoch,
                    recipe_generation: crate::plan_cache::recipe_generation(&home, id),
                    unit: UnitView {
                        recipe: recipes.get(request.unit.as_str()).cloned(),
                        id: request.unit,
                        entry_steps: summary.entries,
                        exit_steps: summary.exits,
                        done: summary.done,
                        settled: summary.settled,
                        steps: views(sql, id, rows.steps, request.compact, &recipes)?,
                    },
                }))
            }
            CommandRequest::PlanHistory(request) => {
                let page_size = limit(request.limit)?;
                let id = messages::resolve_project(sql, &request.project)?;
                let (entries, next_after_seq) =
                    plans::history(sql, id, request.since_rev, request.after_seq, page_size)?;
                Ok(CommandReply::History(PlanHistoryPage {
                    project: identity(sql, id)?,
                    entries,
                    next_after_seq,
                }))
            }
            CommandRequest::PlanView(request) => {
                let id = messages::resolve_project(sql, &request.project)?;
                Ok(CommandReply::Data(
                    serde_json::json!(plan_view(sql, &home, id, request)?).try_into()?,
                ))
            }
            _ => unreachable!("checked plan read command"),
        })
        .await
        .map_err(public)?;
    Ok(Some(reply))
}

struct UnitSummary {
    entries: Vec<StepId>,
    exits: Vec<StepId>,
    done: bool,
    settled: bool,
}
/// A unit's entry and exit steps, `done` and `settled` (SPEC §6.7), from the cached compiled
/// plan and the stored state of its members and of what they read or wait on: no declaration
/// is decoded while the cache is warm, and no step outside the unit's neighbourhood is read.
fn unit_summary(
    sql: &Connection,
    project: ProjectId,
    unit: &UnitName,
    catalog: &Catalog,
    cache: &PlanCache,
) -> sluice_store::Result<UnitSummary> {
    let generation = catalog.generation();
    let (_, plan) = cache.current(
        sql,
        project,
        generation,
        &catalog.for_project(Some(project)),
    )?;
    let found = plan
        .units()
        .get(unit)
        .ok_or_else(|| PublicError::from(PlanRowsError::NoUnit { unit: unit.clone() }))?;
    let mut steps: indexmap::IndexSet<StepId> = found.steps.iter().cloned().collect();
    for id in &found.steps {
        steps.extend(plan.dependencies(id).iter().cloned());
    }
    let scoped = plans::read_scoped_state(
        sql,
        project,
        &PreparationReads {
            steps: steps.into_iter().collect(),
            inputs: plan.inputs().keys().cloned().collect(),
            ..PreparationReads::default()
        },
    )?;
    Ok(UnitSummary {
        entries: found.entries.clone(),
        exits: found.exits.clone(),
        done: found.done(&scoped.state),
        settled: found.settled(&plan, &scoped.state),
    })
}

/// `plan_view`'s text for the project (plan-rows §7.9): the selected steps from the graph
/// index, each step outside the selection an edge joins drawn as a boundary node, and done units
/// left out (and counted) unless `all`. The tool and the board's `?format=mermaid` both draw it.
pub(crate) fn plan_view(
    sql: &Connection,
    home: &Path,
    id: ProjectId,
    request: PlanViewQuery,
) -> sluice_store::Result<String> {
    let recipes = recipes(sql, home, id)?;
    let filter = PlanReadFilter {
        units: request.units,
        steps: request.steps,
        status: request.status,
        recipe: request.recipe,
    };
    let mut selected = row_selection(&filter, &recipes);
    let omitted = if request.all {
        String::new()
    } else {
        // A done unit has no member outside succeeded/skipped. Compact columns suffice.
        let rows = plans::read_steps(sql, id, &RowSelection::default(), StepProjection::Compact)?;
        let mut done = BTreeMap::<UnitName, (bool, usize)>::new();
        for row in &rows.steps {
            let entry = done.entry(row.unit.clone()).or_insert((true, 0));
            entry.0 &= matches!(row.status, StepStatus::Succeeded | StepStatus::Skipped);
            entry.1 += 1;
        }
        let done_count = done.values().filter(|(is_done, _)| *is_done).count();
        let steps_count: usize = done
            .values()
            .filter(|(is_done, _)| *is_done)
            .map(|(_, n)| n)
            .sum();
        let kept: Vec<_> = done
            .into_iter()
            .filter(|(_, (is_done, _))| !is_done)
            .map(|(u, _)| u)
            .collect();
        selected.units = Some(match selected.units {
            Some(units) => units.into_iter().filter(|u| kept.contains(u)).collect(),
            None => kept,
        });
        if done_count == 0 {
            String::new()
        } else {
            format!("{done_count} done units ({steps_count} steps) left out")
        }
    };
    let graph = plans::read_graph(sql, id, &selected)?;
    Ok(render_graph(
        identity(sql, id)?.name.as_str(),
        &graph,
        request.format,
        &omitted,
    ))
}
