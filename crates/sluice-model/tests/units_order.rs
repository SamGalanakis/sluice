//! The units view orders running units by how long they have run, exactly: two units started
//! within a second of each other keep one order as the clock ticks, rather than swapping each
//! time their whole-second ages round level.
mod support;

use indexmap::IndexMap;
use serde_json::json;
use sluice_model::{
    FnSignature, StateSnapshot, StepState,
    commands::StepStatus,
    ids::StepId,
    status::{StepFacts, units_view},
};

fn id(s: &str) -> StepId {
    s.parse().unwrap()
}

#[test]
fn running_units_keep_their_order_as_the_clock_ticks() {
    let signatures = IndexMap::from([(
        "work".to_string(),
        FnSignature {
            open: true,
            ..Default::default()
        },
    )]);
    // `b` comes first in the plan; `a` started 0.45 s earlier.
    let document = json!({"steps":{
        "b-work":{"run":"work","tags":["unit:b"]},
        "a-work":{"run":"work","tags":["unit:a"]}}});
    let plan = support::compile(document, &signatures).unwrap();
    let mut state = StateSnapshot::default();
    for step in ["a-work", "b-work"] {
        state.steps.insert(
            id(step),
            StepState {
                status: StepStatus::Running,
                ..Default::default()
            },
        );
    }
    // A tick where the whole seconds differ (100 vs 99) and one where they round level (100
    // vs 100): the order is the same.
    for (a, b) in [(100.45, 99.9), (100.95, 100.4)] {
        let facts = IndexMap::from([
            (
                id("a-work"),
                StepFacts {
                    running_for: Some(a),
                    ..Default::default()
                },
            ),
            (
                id("b-work"),
                StepFacts {
                    running_for: Some(b),
                    ..Default::default()
                },
            ),
        ]);
        let view = units_view(&plan, &state, &facts, &IndexMap::new(), None, false, None);
        let units: Vec<&str> = view.rows.iter().map(|r| r.unit.as_str()).collect();
        assert_eq!(units, ["a", "b"], "ages {a} and {b}");
        assert_eq!(view.rows[0].age, Some(a as i64));
    }
}
