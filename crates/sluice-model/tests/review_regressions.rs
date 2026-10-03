use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    FnSignature, GateDecision, Plan, StateSnapshot, StepState,
    commands::{CommandRequest, StepStatus},
    edit::{self, EditSnapshot, PlanEdit},
    gates::{CachedResources, evaluate_step, reconcile},
    ids::{Revision, StepId},
    plan::{self, Snapshot, inputs_hash},
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
    Plan::parse(&map(v), &signatures()).unwrap()
}
fn command(name: &str, mut args: Value) -> PlanEdit {
    args["project"] = json!({"kind":"name", "value":"review"});
    args["edit"] = json!({"expected":null,"dry_run":false,"reason":"review","author":null});
    let request: CommandRequest =
        decode_json(&serde_json::to_vec(&json!({"command":name,"args":args})).unwrap()).unwrap();
    request.try_into().unwrap()
}
fn prepare(
    p: &Plan,
    state: &StateSnapshot,
    e: PlanEdit,
) -> Result<edit::PreparedEdit, sluice_model::error::PublicError> {
    edit::prepare_edit(
        &EditSnapshot {
            snapshot: &Snapshot {
                revision: Revision(1),
                document: p.document().clone(),
            },
            state,
            signatures: &signatures(),
            recipes: &IndexMap::new(),
            resources: &CachedResources::default(),
            limits: &IndexMap::new(),
            prune_eligible: None,
        },
        e,
    )
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
    let e = prepare(
        &p,
        &state,
        command("edge_add", json!({"step":"a","after":["flag"]})),
    )
    .unwrap();
    assert!(e.preview.would_stale.is_empty());
    let after = e.plan.compile(&signatures()).unwrap();
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
