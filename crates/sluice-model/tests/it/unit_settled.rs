//! A unit waiting on failed, stale or paused work is at rest; finished work also settles.

use crate::support;

use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    FnSignature, StateSnapshot, StepState,
    commands::StepStatus,
    ids::{StepId, UnitName},
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
    let plan = support::compile(
        document(a_paused),
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
