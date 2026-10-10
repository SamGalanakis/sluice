//! Shared helpers for the plan tests: plans from JSON documents (their rows at positions
//! `0 … n-1`, through the reference's `rows_of`), compiled with `compile_rows`, and edits
//! prepared through the one pipeline with every step's state read.
#![allow(dead_code)]

use indexmap::IndexMap;
use serde_json::Value;
use sluice_model::{
    StateSnapshot,
    ids::Revision,
    plan::{
        EditBase, Plan, PrepareOptions, ResourceLimit, SignatureProvider, compile_rows,
        prepare_plan_edit,
    },
    plan_rows::{
        CatalogGeneration, PlanHeader, PlanOp, PlanRows, PreparedPlanEdit, PreviewScope,
        RecipeGeneration, ScopedState, StateEpoch, ValidationTokens,
    },
    recipe::RecipeEntry,
    rpc::{JsonMap, decode_json},
    types::PathError,
};
use std::sync::Arc;

pub fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
/// A document's rows at positions `0 … n-1`, at rev 1.
pub fn rows(document: &Value) -> PlanRows {
    sluice_reference::ops::rows_of(
        &map(document.clone()),
        PlanHeader {
            rev: Revision(1),
            root_order: vec![],
            state_epoch: StateEpoch(0),
        },
    )
    .unwrap()
}
pub fn compile(
    document: Value,
    signatures: &impl SignatureProvider,
) -> Result<Plan, Vec<PathError>> {
    compile_rows(&rows(&document), signatures)
}
pub fn tokens() -> ValidationTokens {
    ValidationTokens {
        plan_rev: Revision(1),
        state_epoch: StateEpoch(0),
        catalog_generation: CatalogGeneration(1),
        recipe_generation: RecipeGeneration("0000000000000000".into()),
        board_rev: Revision(0),
    }
}
pub fn options() -> PrepareOptions {
    PrepareOptions {
        rev: None,
        dry_run: false,
        preview_scope: PreviewScope::Impact,
        start: true,
        reason: "test".into(),
        author: "test".into(),
    }
}
/// Every base step's state (a stored row for each), as a whole read set.
pub fn whole(plan: &Plan, state: &StateSnapshot) -> ScopedState {
    let mut state = state.clone();
    for id in plan.steps().keys() {
        state.steps.entry(id.clone()).or_default();
    }
    ScopedState {
        state,
        ..Default::default()
    }
}
/// Prepare `ops` on `plan` with every step's state read, no recipes and no resources.
pub fn prepare(
    plan: &Plan,
    state: &StateSnapshot,
    signatures: &impl SignatureProvider,
    ops: Vec<PlanOp>,
    options: PrepareOptions,
) -> Result<PreparedPlanEdit, sluice_model::error::PublicError> {
    let plan = Arc::new(plan.clone());
    let state = whole(&plan, state);
    let tokens = tokens();
    let recipes: IndexMap<String, RecipeEntry> = IndexMap::new();
    let capacities: IndexMap<String, Option<u64>> = IndexMap::new();
    let limits: IndexMap<String, ResourceLimit> = IndexMap::new();
    prepare_plan_edit(
        &EditBase {
            plan: &plan,
            tokens: &tokens,
            state: &state,
            signatures,
            recipes: &recipes,
            capacities: &capacities,
            limits: &limits,
        },
        ops,
        options,
    )
}
