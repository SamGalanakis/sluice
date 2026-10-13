//! The reference's twin of `sluice-model/tests/unit_settled.rs`: a unit is settled (§6.7) only
//! when every one of its steps is settled (§7.1), the same cases with the same answers.

use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_reference::{
    FnSignature, Plan, StateSnapshot, StepState,
    commands::StepStatus,
    ids::{StepId, UnitName},
    rpc::{JsonMap, decode_json},
};

fn id(s: &str) -> StepId {
    s.parse().unwrap()
}

/// `a` outside the unit; unit `y` forks, then works once `a` has finished, then cleans up.
fn document(a_paused: bool) -> Value {
    json!({"steps":{
        "a":{"run":"test.fn","paused":a_paused},
        "y-fork":{"run":"test.fn","tags":["unit:y"]},
        "y-work":{"run":"test.fn","tags":["unit:y","exit"],"after":["a","y-fork"]},
        "y-rm":{"run":"test.fn","tags":["unit:y"],"after":["y-work"]}}})
}

/// Whether unit `y` is settled with these statuses (every other step pending).
fn y_settled(a_paused: bool, statuses: &[(&str, StepStatus)]) -> bool {
    let document: JsonMap = decode_json(&serde_json::to_vec(&document(a_paused)).unwrap()).unwrap();
    let plan = Plan::parse(
        &document,
        &IndexMap::from([("test.fn".to_string(), FnSignature::default())]),
    )
    .unwrap();
    let mut state = StateSnapshot::default();
    for (step, status) in statuses {
        state.steps.insert(
            id(step),
            StepState {
                status: status.clone(),
                ..Default::default()
            },
        );
    }
    plan.units()[&"y".parse::<UnitName>().unwrap()].settled(&plan, &state)
}

#[test]
fn a_unit_waiting_on_a_running_or_startable_step_is_not_settled() {
    use StepStatus::*;
    // The reported case: the fork is done and the work waits on `a`, still running.
    assert!(!y_settled(false, &[("a", Running), ("y-fork", Succeeded)]));
    // `a` can still start.
    assert!(!y_settled(false, &[("y-fork", Succeeded)]));
    // `a` is done: the work is about to start.
    assert!(!y_settled(
        false,
        &[("a", Succeeded), ("y-fork", Succeeded)]
    ));
    assert!(!y_settled(
        false,
        &[("a", Succeeded), ("y-fork", Succeeded), ("y-work", Running)]
    ));
}

#[test]
fn a_unit_waiting_on_a_failed_or_paused_step_or_finished_is_settled() {
    use StepStatus::*;
    assert!(y_settled(false, &[("a", Failed), ("y-fork", Succeeded)]));
    assert!(y_settled(false, &[("a", Stale), ("y-fork", Succeeded)]));
    // `a` is paused: everything behind it is held.
    assert!(y_settled(true, &[("y-fork", Succeeded)]));
    assert!(y_settled(
        false,
        &[
            ("a", Succeeded),
            ("y-fork", Succeeded),
            ("y-work", Succeeded),
            ("y-rm", Succeeded)
        ]
    ));
    assert!(y_settled(
        false,
        &[("a", Succeeded), ("y-fork", Succeeded), ("y-work", Failed)]
    ));
}
