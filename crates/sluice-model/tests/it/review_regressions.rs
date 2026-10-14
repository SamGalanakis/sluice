use crate::support;

use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    FnSignature, GateDecision, Plan, StateSnapshot, StepState,
    commands::{CommandRequest, StepSelection, StepStatus},
    edit::{self, Lowered},
    gates::{evaluate_step, reconcile},
    ids::StepId,
    plan::inputs_hash,
    plan_rows::{PlanOp, PreparedPlanEdit},
    rpc::{JsonMap, decode_json},
};

fn id(s: &str) -> StepId {
    s.parse().unwrap()
}
fn map(v: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&v).unwrap()).unwrap()
}
fn signatures() -> IndexMap<String, FnSignature> {
    IndexMap::from([(
        "work".into(),
        FnSignature {
            open: true,
            ..Default::default()
        },
    )])
}
fn parse(v: Value) -> Plan {
    support::compile(v, &signatures()).unwrap()
}
fn prepare(
    p: &Plan,
    state: &StateSnapshot,
    lowered: Result<Lowered, sluice_model::error::PublicError>,
) -> Result<PreparedPlanEdit, sluice_model::error::PublicError> {
    let lowered = lowered?;
    let plan = std::sync::Arc::new(p.clone());
    let scoped = support::whole(p, state);
    let tokens = support::tokens();
    edit::prepare_lowered(
        &sluice_model::plan::EditBase {
            plan: &plan,
            tokens: &tokens,
            state: &scoped,
            signatures: &signatures(),
            recipes: &IndexMap::new(),
            capacities: &IndexMap::new(),
            limits: &IndexMap::new(),
        },
        lowered,
        support::options(),
    )
}
fn changes(value: Value) -> sluice_model::plan_rows::StepChanges {
    serde_json::from_value(value).unwrap()
}

#[test]
fn gates_and_edits_preserve_effective_inputs() {
    {
        let p = parse(
            json!({"inputs":{"config":{"type":"record","fields":{"enabled":"boolean?"}}},
        "steps":{"gate":{"run":"work","after":["config.enabled"]},
        "data":{"run":"work","in":{"enabled":{"source":"config.enabled"}}}}}),
        );
        let state = StateSnapshot::default();
        let gate = evaluate_step(&p, &state, &p.steps()[&id("gate")]);
        let data = evaluate_step(&p, &state, &p.steps()[&id("data")]);
        assert!(
            matches!(gate, GateDecision::Wait(_)) && matches!(data, GateDecision::Wait(_)),
            "required root is absent: gate={gate:?}, handoff={data:?}"
        );
    }

    {
        let p = parse(json!({"inputs":{"config":"Any?"},
        "steps":{"a":{"run":"work","after":["config.enabled"]}}}));
        let result = evaluate_step(&p, &StateSnapshot::default(), &p.steps()[&id("a")]);
        assert!(
            matches!(result, GateDecision::Skip(_)),
            "optional root is null: {result:?}"
        );
    }

    {
        let p = parse(json!({"steps":{
        "a":{"run":"work","outputs":{"flags":"boolean[]"}},
        "b":{"run":"work","after":["a/flags.0"]}}}));
        let state = StateSnapshot {
            steps: IndexMap::from([(
                id("a"),
                StepState {
                    status: StepStatus::Succeeded,
                    outputs: map(json!({"flags":[]})),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let result = evaluate_step(&p, &state, &p.steps()[&id("b")]);
        assert!(
            matches!(result, GateDecision::Skip(_)),
            "available empty array has no first element: {result:?}"
        );
    }

    {
        let p = parse(json!({"inputs":{"flag":"boolean"},"steps":{"a":{"run":"work"}}}));
        let mut state = StateSnapshot::default();
        state.steps.insert(
            id("a"),
            StepState {
                status: StepStatus::Succeeded,
                inputs_hash: inputs_hash(&p, &state, &p.steps()[&id("a")]),
                ..Default::default()
            },
        );
        let e = prepare(&p, &state, edit::edge(&p, "a", &["flag".into()], true)).unwrap();
        assert!(e.preview.would_stale.is_empty());
        let after = e.compiled.plan;
        assert_eq!(
            reconcile(&after, &state).status(&id("a")),
            StepStatus::Succeeded
        );
    }

    {
        let p = parse(json!({"steps":{"a":{"run":"work","in":{"x":{"default":-0.0}}}}}));
        let state = StateSnapshot {
            steps: IndexMap::from([(
                id("a"),
                StepState {
                    status: StepStatus::Running,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let result = support::prepare(
            &p,
            &state,
            &signatures(),
            vec![PlanOp::StepUpdate {
                step: id("a"),
                changes: Box::new(changes(json!({"in":{"x":{"default":0.0}}}))),
            }],
            support::options(),
        );
        if let Ok(prepared) = &result {
            let after = &prepared.compiled.plan;
            assert_ne!(
                inputs_hash(&p, &state, &p.steps()[&id("a")]),
                inputs_hash(after, &state, &after.steps()[&id("a")])
            );
        }
        assert!(
            result.is_err(),
            "a different effective input hash passed running-step protection"
        );
    }

    {
        let p = parse(json!({"steps":{"a":{"run":"work","in":{"x":{"default":-0.0}}}}}));
        let result = prepare(
            &p,
            &StateSnapshot::default(),
            edit::step_set_input(
                &p,
                &|_| StepStatus::Pending,
                &StepSelection {
                    steps: Some(vec![id("a")]),
                    tags: None,
                },
                &map(json!({"x":0.0})),
            ),
        );
        assert!(
            result.is_ok(),
            "hash-changing edit was refused as a no-op: {result:?}"
        );
    }

    {
        let p = parse(json!({"steps":{"a":{"run":"work","in":{"x":{"default":-0.0}}}}}));
        let mut state = StateSnapshot::default();
        state.steps.insert(
            id("a"),
            StepState {
                status: StepStatus::Succeeded,
                inputs_hash: inputs_hash(&p, &state, &p.steps()[&id("a")]),
                ..Default::default()
            },
        );
        let result = prepare(
            &p,
            &state,
            edit::step_update(&p, &id("a"), &changes(json!({"in":{"x":{"default":0.0}}}))),
        )
        .unwrap();
        assert!(!result.commit.rows.changes.is_empty());
        assert_eq!(result.preview.would_stale, [id("a")]);
    }

    {
        let result = decode_json::<CommandRequest>(
            br#"{"command":"plan_edit","args":{
        "project":{"kind":"name","value":"review"},"rev":1,
        "ops":[{"op":"step.add","step":"a","spec":{"run":"work"}}],"start":false,
        "dry_run":false,"reason":"review","author":null}}"#,
        );
        let Ok(CommandRequest::PlanEdit(edit)) = result else {
            panic!("start=false cannot enter the plan_edit path: {result:?}");
        };
        assert!(!edit.start);
    }
}
