//! Where a schema-3 operation and a schema-1 typed tool mean the same edit, the reference's
//! operation semantics (`ops`, `harness`) and today's typed preparation (`prepare_edit`)
//! agree: both refuse, or both accept with the same candidate document and the same preview.
//! `step.add` is `step_add`, `step.update` is `step_update`, `step.remove` is `step_remove`
//! by ids, `edge.add`/`edge.remove` are `edge_add`/`edge_remove`, and `unit.add` is
//! `unit_add` (with `start: true`; with `start: false` today puts `paused` after the tags, the
//! operation appends it, so only the key order differs).

use proptest::prelude::*;
use serde_json::{Value, json};
use sluice_model::plan_rows::PlanOp;
use sluice_reference::{
    commands::CommandRequest,
    generate::{self, Fixture},
    harness::{Verdict, evaluate},
    ops,
};

/// The typed command that means the same as `op`, if one does.
fn typed(op: &PlanOp, start: bool) -> Option<Value> {
    let (command, args) = match op {
        PlanOp::StepAdd { step, spec } => (
            "step_add",
            json!({"step": step, "spec": spec, "start": start}),
        ),
        PlanOp::StepUpdate { step, changes } => {
            ("step_update", json!({"step": step, "changes": changes}))
        }
        PlanOp::StepRemove { steps } => (
            "step_remove",
            json!({"selection": {"steps": steps, "tags": null}}),
        ),
        PlanOp::EdgeAdd { step, after } => ("edge_add", json!({"step": step, "after": after})),
        PlanOp::EdgeRemove { step, after } => {
            ("edge_remove", json!({"step": step, "after": after}))
        }
        PlanOp::UnitAdd {
            recipe,
            unit,
            params,
            after,
            inputs,
            tags,
        } if start => (
            "unit_add",
            json!({"recipe": recipe, "unit": unit, "params": params, "start": true,
                "after": after, "inputs": inputs, "tags": tags}),
        ),
        _ => return None,
    };
    let mut args = args;
    args["project"] = json!({"kind": "name", "value": "gen"});
    args["edit"] = json!({"expected": null, "dry_run": false, "reason": "typed", "author": null});
    Some(json!({"command": command, "args": args}))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn one_operation_means_what_its_typed_tool_means(case in generate::case(10, 1)) {
        let fixture = Fixture::new();
        let op = &case.ops[0];
        let Some(command) = typed(op, case.start) else {
            return Ok(());
        };
        let request: CommandRequest =
            sluice_model::rpc::decode_json(&serde_json::to_vec(&command).unwrap()).unwrap();
        let plan = sluice_reference::Plan::parse(&ops::document_of(&case.base), &fixture.signatures)
            .unwrap();
        let today = sluice_reference::prepare_edit(
            &sluice_reference::EditSnapshot {
                revision: sluice_model::ids::Revision(1),
                plan: &plan,
                state: &case.state,
                signatures: &fixture.signatures,
                recipes: &fixture.recipes,
                resources: &case.resources,
                limits: &case.limits,
                prune_eligible: None,
            },
            request.try_into().unwrap(),
        );
        let verdict = evaluate(&case, &fixture.context()).unwrap();
        match (&verdict, today) {
            (Verdict::Accepted(ours), Ok(theirs)) => {
                prop_assert_eq!(
                    serde_json::to_string(&ours.document).unwrap(),
                    serde_json::to_string(theirs.plan.document()).unwrap(),
                    "{}", command
                );
                prop_assert_eq!(&ours.all.would_start, &theirs.preview.would_start);
                prop_assert_eq!(&ours.all.would_queue, &theirs.preview.would_queue);
                prop_assert_eq!(&ours.all.would_skip, &theirs.preview.would_skip);
                prop_assert_eq!(&ours.all.would_stale, &theirs.preview.would_stale);
                prop_assert_eq!(&ours.all.errors, &theirs.preview.errors);
                prop_assert_eq!(ours.changes.is_empty(), theirs.ops.is_empty(), "{}", command);
                if matches!(op, PlanOp::UnitAdd { .. }) {
                    prop_assert_eq!(&ours.steps, &theirs.steps);
                }
            }
            (Verdict::Refused(_) | Verdict::Invalid(_), Err(_)) => {}
            (ours, theirs) => prop_assert!(
                false,
                "{}: the operation gives {:?}, the typed tool {:?}",
                command,
                ours.errors(),
                theirs.map(|p| p.ops)
            ),
        }
    }
}
