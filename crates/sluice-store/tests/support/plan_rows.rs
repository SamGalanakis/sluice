//! Hand-built schema-3 plan edits for store tests: a whole document is turned into the
//! `PlanEditCommit` an edit's preparation would hand the writer (its row changes, index rows,
//! removed and added steps and, given the compiled candidate, the status transitions its
//! reconciliation settles). The store tests own no preparation; this stands in for it.

use rusqlite::Connection;
use sluice_model::{
    commands::StepStatus,
    gates::{self, StateSnapshot},
    ids::{ProjectId, Revision, StepId},
    plan::Plan,
    plan_index::{output_references, plan_edges, step_index},
    plan_rows::{
        CatalogGeneration, EdgeRow, PlanChange, PlanEditCommit, PlanRows, RecipeGeneration,
        RowDelta, StateDelta, StatusTransition, ValidationTokens,
    },
    rpc::JsonMap,
};
use sluice_store::{
    WriteTransaction,
    plans::{self, CommitOutcome, PruneEligibility},
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use std::collections::{BTreeSet, HashSet};

/// The tokens the writer would find now.
pub fn tokens(sql: &Connection, project: ProjectId) -> ValidationTokens {
    let header = plans::plan_header(sql, project).unwrap();
    let board_rev: i64 = sql
        .query_row(
            "SELECT board_rev FROM projects WHERE project_id=?1",
            [project.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    ValidationTokens {
        plan_rev: header.rev,
        state_epoch: header.state_epoch,
        catalog_generation: CatalogGeneration(0),
        recipe_generation: RecipeGeneration("0000000000000000".into()),
        board_rev: Revision(board_rev as u64),
    }
}

/// The commit that takes the project's rows to `document`'s. `compiled` is the candidate the
/// transitions are reconciled with (none: no transitions).
pub fn document_commit(
    sql: &Connection,
    project: ProjectId,
    document: &JsonMap,
    compiled: Option<&Plan>,
    author: &str,
    reason: &str,
) -> PlanEditCommit {
    let current = plans::read_plan_rows(sql, project).unwrap();
    let next = PlanRows::from_document(document, Some(&current)).unwrap();
    rows_commit(sql, project, &current, &next, compiled, author, reason)
}

/// The commit from `current` to `next` (rows of the same project).
pub fn rows_commit(
    sql: &Connection,
    project: ProjectId,
    current: &PlanRows,
    next: &PlanRows,
    compiled: Option<&Plan>,
    author: &str,
    reason: &str,
) -> PlanEditCommit {
    let changes = next.changes_from(Some(current));
    let ids: HashSet<&str> = next.steps.iter().map(|r| r.step.as_str()).collect();
    let is_step = |name: &str| ids.contains(name);
    let mut step_index_rows = vec![];
    let mut output_refs = vec![];
    for change in &changes {
        match change {
            PlanChange::StepPut {
                step, declaration, ..
            } => step_index_rows.push(step_index(step, declaration, &is_step)),
            PlanChange::OutputPut { name, binding, .. } => {
                output_refs.push((name.clone(), output_references(name, binding)))
            }
            _ => {}
        }
    }
    let incoming = |rows: &PlanRows| -> Vec<(StepId, BTreeSet<String>, Vec<EdgeRow>)> {
        let edges = plan_edges(rows);
        rows.steps
            .iter()
            .map(|row| {
                let mine: Vec<EdgeRow> = edges
                    .iter()
                    .filter(|e| e.target == row.step)
                    .cloned()
                    .collect();
                let set = mine.iter().map(|e| format!("{e:?}")).collect();
                (row.step.clone(), set, mine)
            })
            .collect()
    };
    let before = incoming(current);
    let put: HashSet<&StepId> = step_index_rows.iter().map(|i| &i.step).collect();
    let edges = incoming(next)
        .into_iter()
        .filter(|(step, set, _)| {
            put.contains(step)
                || before
                    .iter()
                    .find(|(s, ..)| s == step)
                    .is_none_or(|(_, old, _)| old != set)
        })
        .map(|(step, _, edges)| (step, edges))
        .collect();
    let old: HashSet<&StepId> = current.steps.iter().map(|r| &r.step).collect();
    let new: HashSet<&StepId> = next.steps.iter().map(|r| &r.step).collect();
    let removed: Vec<StepId> = current
        .steps
        .iter()
        .filter(|r| !new.contains(&r.step))
        .map(|r| r.step.clone())
        .collect();
    let added: Vec<StepId> = next
        .steps
        .iter()
        .filter(|r| !old.contains(&r.step))
        .map(|r| r.step.clone())
        .collect();
    let transitions = match compiled {
        None => vec![],
        Some(plan) if !changes.is_empty() => {
            let state = plans::read_state(sql, project).unwrap();
            let projected = StateSnapshot {
                paused: state.paused.clone(),
                inputs: JsonMap(
                    plan.inputs()
                        .keys()
                        .filter_map(|name| Some((name.clone(), state.inputs.0.get(name)?.clone())))
                        .collect(),
                ),
                steps: plan
                    .steps()
                    .keys()
                    .map(|id| (id.clone(), state.steps.get(id).cloned().unwrap_or_default()))
                    .collect(),
            };
            gates::reconcile(plan, &projected)
                .steps
                .into_iter()
                .filter_map(|(id, entry)| {
                    let from = projected.status(&id);
                    let old = projected.steps.get(&id);
                    if from == entry.status
                        && old.is_some_and(|o| o.skipped == entry.skipped && o.error == entry.error)
                    {
                        return None;
                    }
                    Some(StatusTransition {
                        step: id,
                        from,
                        to: entry.status,
                        skipped: entry.skipped,
                        error: entry.error,
                    })
                })
                .collect()
        }
        Some(_) => vec![],
    };
    PlanEditCommit {
        tokens: tokens(sql, project),
        rev: None,
        author: author.into(),
        reason: reason.into(),
        rows: RowDelta {
            changes,
            step_index: step_index_rows,
            output_refs,
            edges,
        },
        state: StateDelta {
            removed,
            added,
            transitions,
        },
        prune: None,
    }
}

/// Create a project named `name` (rev 1, the three empty sections).
pub fn create_project(
    tx: &mut WriteTransaction<'_>,
    name: &str,
) -> sluice_store::Result<ProjectId> {
    Ok(projects::project_create(
        tx,
        CreateProject {
            name: name.parse().unwrap(),
            description: String::new(),
            icon: None,
            resources: None,
            author: "sam".into(),
        },
        &EmptyPlanInitializer,
        &NoResourceSettings,
    )?
    .project_id)
}

/// Commit `document` as the project's next revision, reconciled with `compiled`, in `tx`.
pub fn commit_document(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    document: &JsonMap,
    compiled: Option<&Plan>,
    prune: Option<&PruneEligibility>,
) -> sluice_store::Result<Revision> {
    let commit = document_commit(tx.sql(), project, document, compiled, "sam", "change");
    match plans::commit_plan_edit(tx, project, &commit, prune)? {
        CommitOutcome::Committed(rev) => Ok(rev),
        CommitOutcome::Stale => panic!("a commit built in its own transaction is never stale"),
    }
}

/// The status a step reads, for assertions.
#[allow(dead_code)]
pub fn status(sql: &Connection, project: ProjectId, step: &str) -> StepStatus {
    plans::read_state(sql, project)
        .unwrap()
        .status(&step.parse().unwrap())
}
