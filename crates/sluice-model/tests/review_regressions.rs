mod support;

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
fn missing_required_record_must_wait_before_optional_field_navigation() {
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

#[test]
fn missing_optional_any_ancestor_is_null_not_waiting() {
    let p = parse(json!({"inputs":{"config":"Any?"},
        "steps":{"a":{"run":"work","after":["config.enabled"]}}}));
    let result = evaluate_step(&p, &StateSnapshot::default(), &p.steps()[&id("a")]);
    assert!(
        matches!(result, GateDecision::Skip(_)),
        "optional root is null: {result:?}"
    );
}

#[test]
fn missing_index_of_available_boolean_array_is_null_not_waiting() {
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

#[test]
fn adding_boolean_gate_does_not_stale_completed_work() {
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

#[test]
fn float_input_hash_survives_json_storage_roundtrip() {
    use sluice_model::hash::InputsHash;
    let mut seed = 42_u64;
    for _ in 0..10000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let value = f64::from_bits(seed);
        if !value.is_finite() {
            continue;
        }
        let first: JsonMap =
            decode_json(&serde_json::to_vec(&json!({"x":value})).unwrap()).unwrap();
        let bytes = serde_json::to_vec(&first).unwrap();
        let second: JsonMap = decode_json(&bytes).unwrap();
        assert_eq!(
            InputsHash::of(&first).unwrap(),
            InputsHash::of(&second).unwrap(),
            "stored JSON {} changed value on read: first={first:?} second={second:?}",
            String::from_utf8(bytes).unwrap()
        );
    }
}

#[test]
fn running_step_cannot_change_signed_zero_input() {
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

#[test]
fn bulk_input_signed_zero_change_must_not_be_discarded() {
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

#[test]
fn step_update_signed_zero_change_is_preserved_and_stales_success() {
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

#[test]
fn unit_add_wire_accepts_staging_tags_and_inputs() {
    let result = decode_json::<CommandRequest>(
        br#"{"command":"unit_add","args":{
        "project":{"kind":"name","value":"review"},"recipe":"pair","unit":"u","params":{},
        "start":true,"after":{},"tags":["arc:review"],"inputs":{"work":{"model":"chosen"}},
        "edit":{"expected":null,"dry_run":false,"reason":"review","author":null}}}"#,
    );
    assert!(
        result.is_ok(),
        "documented staging fields cannot enter the model: {result:?}"
    );
}

#[test]
fn step_add_wire_accepts_start_false() {
    let result = decode_json::<CommandRequest>(
        br#"{"command":"step_add","args":{
        "project":{"kind":"name","value":"review"},"step":"a","spec":{"run":"work"},"start":false,
        "edit":{"expected":null,"dry_run":false,"reason":"review","author":null}}}"#,
    );
    assert!(
        result.is_ok(),
        "start=false cannot enter the step-add path: {result:?}"
    );
}

#[test]
fn plan_edit_wire_accepts_start_false() {
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

#[test]
fn prune_wire_accepts_tag_selection() {
    let result = decode_json::<CommandRequest>(br#"{"command":"plan_prune","args":{
        "project":{"kind":"name","value":"review"},"units":null,"tags":["arc:review"],"older_than_seconds":0,
        "edit":{"expected":null,"dry_run":false,"reason":"review","author":null}}}"#);
    assert!(
        result.is_ok(),
        "tag selection cannot enter the prune path: {result:?}"
    );
}
