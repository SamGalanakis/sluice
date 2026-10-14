//! A plan for the dashboard's tests and its fixture, built the way a project's plan is: rows
//! committed as one plan edit through the store (`commit_plan_edit`), with the index rows the
//! model derives from the declarations (plan-rows §2.5), never a stored document. Fixtures put
//! a plan right after `project_create`, then set statuses, runs and errors on the step rows.
#![allow(dead_code)]
use serde_json::Value;
use sluice_model::{
    ids::{ProjectId, Revision, StepId, UnitName},
    plan::{FnSignature, Plan, SignatureProvider, compile_rows, output_references, step_index},
    plan_rows::{
        CatalogGeneration, EdgeKind, EdgeRow, PlanChange, PlanEditCommit, PlanRows,
        RecipeGeneration, RefKind, RowDelta, SourceKind, StateDelta, StepIndexRows,
        ValidationTokens,
    },
    rpc::JsonMap,
};
use sluice_store::{
    WriteTransaction,
    plans::{self, CommitOutcome},
};

/// Every fn open: a fixture's steps declare their own outputs.
pub struct Open;
impl SignatureProvider for Open {
    fn signature(&self, _: &str) -> Option<FnSignature> {
        Some(FnSignature {
            open: true,
            ..Default::default()
        })
    }
}

fn document(doc: Value) -> JsonMap {
    serde_json::from_value(doc).expect("a plan document is a JSON object")
}

/// `doc` compiled from its rows against `signatures`, as a page's plan is: for tests that draw a
/// step from a plan without a home.
pub fn compile(doc: Value, signatures: &impl SignatureProvider) -> Plan {
    compile_rows(
        &PlanRows::from_document(&document(doc), None).expect("the document converts to rows"),
        signatures,
    )
    .unwrap_or_else(|e| panic!("the plan compiles: {e:?}"))
}

/// Make `doc` `project`'s plan in one edit by the owner: each section, input, output and step
/// that differs from the stored rows is put, each one it lacks is deleted, and the index rows
/// (units, tags, references and edges) of what it puts are written beside them. Statuses are
/// the store's: an added step is pending.
pub fn put(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    doc: Value,
) -> sluice_store::Result<Revision> {
    put_by(tx, project, doc, "owner", "")
}

/// `put`, by `author` for `reason`.
pub fn put_by(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    doc: Value,
    author: &str,
    reason: &str,
) -> sluice_store::Result<Revision> {
    let base = plans::read_plan_rows(tx.sql(), project)?;
    let rows = PlanRows::from_document(&document(doc), Some(&base))
        .expect("the document converts to rows");
    let plan = compile_rows(&rows, &Open).unwrap_or_else(|e| panic!("the plan compiles: {e:?}"));
    let mut changes = vec![];
    if rows.header.root_order != base.header.root_order {
        changes.push(PlanChange::HeaderPut {
            root_order: rows.header.root_order.clone(),
        });
    }
    // deletes by key, then puts by position (plan-rows §5.1)
    let mut deleted: Vec<String> = base
        .inputs
        .iter()
        .filter(|b| !rows.inputs.iter().any(|r| r.name == b.name))
        .map(|b| b.name.clone())
        .collect();
    deleted.sort();
    changes.extend(
        deleted
            .into_iter()
            .map(|name| PlanChange::InputDelete { name }),
    );
    let mut deleted: Vec<String> = base
        .outputs
        .iter()
        .filter(|b| !rows.outputs.iter().any(|r| r.name == b.name))
        .map(|b| b.name.clone())
        .collect();
    deleted.sort();
    changes.extend(
        deleted
            .into_iter()
            .map(|name| PlanChange::OutputDelete { name }),
    );
    let mut removed: Vec<StepId> = base
        .steps
        .iter()
        .filter(|b| !rows.steps.iter().any(|r| r.step == b.step))
        .map(|b| b.step.clone())
        .collect();
    removed.sort();
    changes.extend(
        removed
            .iter()
            .map(|step| PlanChange::StepDelete { step: step.clone() }),
    );
    for row in &rows.inputs {
        if !base.inputs.contains(row) {
            changes.push(PlanChange::InputPut {
                name: row.name.clone(),
                position: row.position,
                declaration: row.declaration.clone(),
            });
        }
    }
    let mut output_refs = vec![];
    for row in &rows.outputs {
        if !base.outputs.contains(row) {
            changes.push(PlanChange::OutputPut {
                name: row.name.clone(),
                position: row.position,
                binding: row.binding.clone(),
            });
            output_refs.push((row.name.clone(), output_references(&row.name, &row.binding)));
        }
    }
    let is_step = |name: &str| rows.steps.iter().any(|s| s.step.as_str() == name);
    let mut index = vec![];
    let mut added = vec![];
    for row in &rows.steps {
        if !base.steps.contains(row) {
            changes.push(PlanChange::StepPut {
                step: row.step.clone(),
                position: row.position,
                declaration: row.declaration.clone(),
            });
            index.push(step_index(&row.step, &row.declaration, &is_step));
            if !base.steps.iter().any(|b| b.step == row.step) {
                added.push(row.step.clone());
            }
        }
    }
    let every: Vec<StepIndexRows> = rows
        .steps
        .iter()
        .map(|row| step_index(&row.step, &row.declaration, &is_step))
        .collect();
    let edges = every
        .iter()
        .map(|step| (step.step.clone(), incoming(step, &plan)))
        .collect();
    let board_rev: i64 = tx.sql().query_row(
        "SELECT board_rev FROM projects WHERE project_id=?1",
        [project.to_string()],
        |r| r.get(0),
    )?;
    let commit = PlanEditCommit {
        tokens: ValidationTokens {
            plan_rev: base.header.rev,
            state_epoch: base.header.state_epoch,
            catalog_generation: CatalogGeneration::default(),
            recipe_generation: RecipeGeneration(String::new()),
            board_rev: Revision(board_rev as u64),
        },
        rev: Some(base.header.rev),
        author: author.into(),
        reason: reason.into(),
        rows: RowDelta {
            changes,
            step_index: index,
            output_refs,
            edges,
        },
        state: StateDelta {
            removed,
            added: added.clone(),
            transitions: vec![],
        },
        prune: None,
    };
    let rev = match plans::commit_plan_edit(tx, project, &commit, None)? {
        CommitOutcome::Committed(rev) => rev,
        CommitOutcome::Stale => panic!("a fixture's plan edit went stale"),
    };
    // The runs and attempts a test inserts by hand are of generation 1 (the columns' default):
    // the steps this put adds are made of it too, as a plan put at rev 1 once made them.
    for step in &added {
        tx.sql().execute(
            "UPDATE steps SET generation=1 WHERE project_id=?1 AND step_id=?2",
            (project.to_string(), step.as_str()),
        )?;
    }
    Ok(rev)
}

/// A step's incoming `plan_edges` (plan-rows §2.5): a data edge from each step its bindings
/// read, a gate edge from each step its gates name or read a boolean from, and for each
/// `unit:u` gate a gate edge from each exit step of `u`.
fn incoming(step: &StepIndexRows, plan: &Plan) -> Vec<EdgeRow> {
    let mut edges: Vec<EdgeRow> = vec![];
    let mut add = |edge: EdgeRow| {
        if !edges.contains(&edge) {
            edges.push(edge);
        }
    };
    for reference in &step.references {
        match reference.source_kind {
            SourceKind::Step => add(EdgeRow {
                source: reference.source_id.parse().expect("a step id"),
                target: step.step.clone(),
                kind: match reference.kind {
                    RefKind::Binding => EdgeKind::Data,
                    RefKind::Gate | RefKind::Output => EdgeKind::Gate,
                },
                via_unit: None,
            }),
            SourceKind::Unit => {
                let unit: UnitName = reference.source_id.parse().expect("a unit name");
                for exit in plan
                    .units()
                    .get(&unit)
                    .map(|u| u.exits.as_slice())
                    .unwrap_or_default()
                {
                    add(EdgeRow {
                        source: exit.clone(),
                        target: step.step.clone(),
                        kind: EdgeKind::Gate,
                        via_unit: Some(unit.clone()),
                    });
                }
            }
            SourceKind::Input => {}
        }
    }
    edges
}
