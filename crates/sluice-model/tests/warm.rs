//! Warm edits: a chain of edits, each prepared on the candidate the one before certified,
//! compiles no plan whole (`full_compiles` stays 0); a step added with a fn the base never used
//! compiles into the candidate; and preparing from a state that lacks a step the edit consults
//! panics in a debug build. Its own test binary, so no other test moves the process-wide
//! counter while it counts.

mod support;

use serde_json::json;
use sluice_model::{
    StateSnapshot,
    ids::StepId,
    plan::{EditBase, full_compiles, prepare_plan_edit, reset_counters},
    plan_rows::{PlanOp, ScopedState},
};
use sluice_reference::{generate, interop::model_signatures};
use std::sync::Arc;

fn op(value: serde_json::Value) -> PlanOp {
    serde_json::from_value(value).unwrap()
}
fn id(name: &str) -> StepId {
    name.parse().unwrap()
}

fn base_document() -> serde_json::Value {
    json!({
        "inputs": {"n": "int"},
        "steps": {
            "a": {"run": "t.add", "in": {"a": {"source": "n"}, "b": {"default": 1}}},
            "b": {"run": "t.add", "in": {"a": {"source": "a/sum"}, "b": {"default": 2}}},
            "c": {"run": "t.echo", "in": {"value": {"source": "b/sum"}}},
        },
        "outputs": {"total": {"source": "b/sum"}},
    })
}

#[test]
fn warm_edits_compile_no_plan_whole() {
    let signatures = model_signatures(&generate::catalog());
    let mut plan = Arc::new(support::compile(base_document(), &signatures).unwrap());
    let state = StateSnapshot::default();
    reset_counters();
    let edits = [
        vec![op(json!({"op": "step.add", "step": "d",
            "spec": {"run": "t.late", "in": {"x": {"source": "a/sum"}}}}))],
        vec![op(json!({"op": "step.update", "step": "b",
            "changes": {"in": {"a": {"source": "d/y"}, "b": {"default": 3}}}}))],
        vec![op(
            json!({"op": "output.put", "name": "late", "source": "d/y"}),
        )],
        vec![op(json!({"op": "edge.add", "step": "c", "after": ["a"]}))],
        vec![op(json!({"op": "step.remove", "steps": ["c"]}))],
        vec![op(
            json!({"op": "input.put", "name": "m", "declaration": "int?"}),
        )],
    ];
    for (index, ops) in edits.into_iter().enumerate() {
        let prepared = support::prepare(&plan, &state, &signatures, ops, support::options())
            .unwrap_or_else(|error| panic!("edit {index}: {error:?}"));
        plan = prepared.compiled.plan.clone();
    }
    assert_eq!(full_compiles(), 0, "a warm edit compiled a plan whole");
    assert!(plan.steps().contains_key(&id("d")));
    assert!(!plan.steps().contains_key(&id("c")));
    assert!(plan.outputs().contains_key("late"));
}

/// `t.late` is in the catalog but no base step runs it: the edit's validation reads its
/// signature from the provider, not from what the base already compiled.
#[test]
fn a_step_with_a_fn_the_base_never_used_compiles() {
    let signatures = model_signatures(&generate::catalog());
    let plan = support::compile(base_document(), &signatures).unwrap();
    assert!(plan.steps().values().all(|step| step.run != "t.late"));
    let prepared = support::prepare(
        &plan,
        &StateSnapshot::default(),
        &signatures,
        vec![op(json!({"op": "step.add", "step": "late",
            "spec": {"run": "t.late", "in": {"x": {"source": "a/sum"}}}}))],
        support::options(),
    )
    .unwrap();
    let late = &prepared.compiled.plan.steps()[&id("late")];
    assert_eq!(late.run, "t.late");
    assert_eq!(prepared.compiled.plan.dependencies(&id("late")), [id("a")]);
    // A wrong type against the fn the base never used is refused, so its signature was read.
    let refused = support::prepare(
        &plan,
        &StateSnapshot::default(),
        &signatures,
        vec![op(json!({"op": "step.add", "step": "late",
            "spec": {"run": "t.late", "in": {"x": {"default": "text"}}}}))],
        support::options(),
    );
    assert!(refused.is_err(), "{refused:?}");
}

/// A state read without the steps the edit reaches: preparation notices it consulted a step
/// outside its read set and, in a debug build, panics.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "outside its read set")]
fn preparation_outside_its_read_set_panics_in_debug() {
    let signatures = model_signatures(&generate::catalog());
    let plan = Arc::new(support::compile(base_document(), &signatures).unwrap());
    let state = ScopedState::default();
    let tokens = support::tokens();
    let none = Default::default();
    let capacities = Default::default();
    let limits = Default::default();
    let _ = prepare_plan_edit(
        &EditBase {
            plan: &plan,
            tokens: &tokens,
            state: &state,
            signatures: &signatures,
            recipes: &none,
            capacities: &capacities,
            limits: &limits,
        },
        vec![op(json!({"op": "step.update", "step": "a",
            "changes": {"in": {"a": {"source": "n"}, "b": {"default": 5}}}}))],
        support::options(),
    );
}
