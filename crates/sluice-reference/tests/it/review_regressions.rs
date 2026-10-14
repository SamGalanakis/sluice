//! Ported from `sluice-model/tests/review_regressions.rs` at `775d57c`: the same checks, run against the reference.
use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_reference::{
    FnSignature, GateDecision, Plan, StateSnapshot, StepState,
    commands::{CommandRequest, StepStatus},
    edit::{self, EditSnapshot, PlanEdit},
    gates::{CachedResources, evaluate_step},
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
                ops: decode_json(
                    br#"[{"op":"replace","path":"/steps/a/in/x/default","value":0.0}]"#,
                )
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
}
