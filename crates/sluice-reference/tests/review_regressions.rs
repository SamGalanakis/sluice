//! Ported from `sluice-model/tests/review_regressions.rs` at `775d57c`: the same checks, run against the reference.
use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_reference::{
    FnSignature, GateDecision, Plan, StateSnapshot, StepState,
    commands::{CommandRequest, StepStatus},
    edit::{self, EditSnapshot, PlanEdit},
    gates::{CachedResources, evaluate_step, reconcile},
    ids::{Revision, StepId},
    plan::inputs_hash,
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
) -> Result<edit::PreparedEdit, sluice_reference::error::PublicError> {
    edit::prepare_edit(
        &EditSnapshot {
            revision: Revision(1),
            plan: p,
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
    let after = e.plan;
    assert_eq!(
        reconcile(&after, &state).status(&id("a")),
        StepStatus::Succeeded
    );
}

#[test]
fn float_input_hash_survives_json_storage_roundtrip() {
    use sluice_reference::hash::InputsHash;
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
    let result = prepare(
        &p,
        &state,
        PlanEdit::Patch(sluice_reference::commands::PlanPatch {
            project: "review".parse().unwrap(),
            rev: Revision(1),
            start: true,
            dry_run: false,
            author: None,
            reason: "review".into(),
            ops: decode_json(br#"[{"op":"replace","path":"/steps/a/in/x/default","value":0.0}]"#)
                .unwrap(),
        }),
    );
    if let Ok(prepared) = &result {
        let after = &prepared.plan;
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
        command(
            "step_set_input",
            json!({
                "selection":{"steps":["a"],"tags":null},"inputs":{"x":0.0}
            }),
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
        command(
            "step_update",
            json!({
                "step":"a","changes":{"in":{"x":{"default":0.0}}}
            }),
        ),
    )
    .unwrap();
    assert!(!result.ops.is_empty());
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
fn plan_patch_wire_accepts_start_false() {
    let result = decode_json::<CommandRequest>(
        br#"{"command":"plan_patch","args":{
        "project":{"kind":"name","value":"review"},"rev":1,
        "ops":[{"op":"add","path":"/steps/a","value":{"run":"work"}}],"start":false,
        "dry_run":false,"reason":"review","author":null}}"#,
    );
    assert!(
        result.is_ok(),
        "start=false cannot enter the patch path: {result:?}"
    );
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
