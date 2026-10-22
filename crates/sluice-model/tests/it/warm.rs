//! A chain of warm edits compiles no whole plan. A new fn is validated against its
//! catalog signature. The cost scenario runs in a child of this test binary so other
//! libtest threads cannot move its process-wide counters.

use crate::support;

use serde_json::json;
use sluice_model::{StateSnapshot, cost::Measurement, ids::StepId, plan_rows::PlanOp};
use sluice_reference::{generate, interop::model_signatures};
use std::sync::Arc;

fn op(value: serde_json::Value) -> PlanOp {
    serde_json::from_value(value).unwrap()
}
fn id(name: &str) -> StepId {
    name.parse().unwrap()
}
/// One test at a time: `full_compiles` counts every compile in the process.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TESTS.lock().unwrap_or_else(|e| e.into_inner())
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
    if std::env::var_os("SLUICE_WARM_COST_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "warm::slow_warm_edits_compile_no_plan_whole",
                "--nocapture",
            ])
            .env("SLUICE_WARM_COST_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _serial = serial();
    let signatures = model_signatures(&generate::catalog());
    let mut plan = Arc::new(support::compile(base_document(), &signatures).unwrap());
    let state = StateSnapshot::default();
    let measurement = Measurement::start();
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
    assert_eq!(
        measurement.costs().full_compiles,
        0,
        "a warm edit compiled a plan whole"
    );
    assert!(plan.steps().contains_key(&id("d")));
    assert!(!plan.steps().contains_key(&id("c")));
    assert!(plan.outputs().contains_key("late"));
}

/// `t.late` is in the catalog but no base step runs it: the edit's validation reads its
/// signature from the provider, not from what the base already compiled.
#[test]
fn a_step_with_a_fn_the_base_never_used_compiles() {
    let _serial = serial();
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
