//! The reference meaning of each operation, case by case: where a put adds a section, where a
//! changed key goes, positions and net changes, refusals and their paths, unit targets,
//! recipe expansion and `start: false`.

use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::plan_rows::{PlanChange, PlanOp, RootSection};
use sluice_reference::{
    generate::{self, Fixture},
    harness::{EditCase, Open, Verdict, evaluate},
    ops,
};

fn map(value: Value) -> sluice_model::rpc::JsonMap {
    serde_json::from_value(value).unwrap()
}
fn op_list(value: Value) -> Vec<PlanOp> {
    serde_json::from_value(value).unwrap()
}
/// Apply `ops` to `base` with `Open` signatures and the generated recipes.
fn apply(base: Value, ops: Value, start: bool) -> Result<ops::Applied, Vec<String>> {
    let case = EditCase::new(&map(base), op_list(ops)).unwrap();
    ops::apply(&case.base, &case.ops, start, &generate::recipes(), &Open)
        .map_err(|e| e.iter().map(ToString::to_string).collect())
}
fn exported(applied: &ops::Applied) -> String {
    serde_json::to_string(&ops::document_of(&applied.rows)).unwrap()
}

#[test]
fn a_put_into_an_absent_section_adds_it_at_its_canonical_place() {
    let applied = apply(
        json!({"steps": {}, "inputs": {"a": "int"}}),
        json!([{"op": "output.put", "name": "o", "source": "a"}]),
        true,
    )
    .unwrap();
    assert_eq!(
        applied.rows.header.root_order,
        [
            RootSection::Outputs,
            RootSection::Steps,
            RootSection::Inputs
        ]
    );
    let applied = apply(
        json!({"outputs": {}}),
        json!([{"op": "input.put", "name": "a", "declaration": "int"}]),
        true,
    )
    .unwrap();
    assert_eq!(exported(&applied), r#"{"inputs":{"a":"int"},"outputs":{}}"#);
    let base = EditCase::new(&map(json!({"outputs": {}})), vec![])
        .unwrap()
        .base;
    assert_eq!(
        ops::changes(&base, &applied.rows),
        [
            PlanChange::HeaderPut {
                root_order: vec![RootSection::Inputs, RootSection::Outputs]
            },
            PlanChange::InputPut {
                name: "a".into(),
                position: 0,
                declaration: serde_json::from_value(json!("int")).unwrap()
            }
        ]
    );
}

#[test]
fn a_changed_key_keeps_its_place_a_new_one_is_appended_and_null_removes() {
    let applied = apply(
        json!({"steps": {"a": {"run": "x", "doc": "d", "priority": 1, "tags": ["t"]}}}),
        json!([{"op": "step.update", "step": "a",
            "changes": {"priority": 2, "doc": null, "paused": true}}]),
        true,
    )
    .unwrap();
    assert_eq!(
        exported(&applied),
        r#"{"steps":{"a":{"run":"x","priority":2,"tags":["t"],"paused":true}}}"#
    );
}

#[test]
fn removing_and_adding_an_id_again_is_one_change_at_a_new_position() {
    let base = json!({"steps": {"a": {"run": "x"}, "b": {"run": "x"}, "c": {"run": "x"}}});
    let case = EditCase::new(
        &map(base),
        op_list(json!([
            {"op": "step.remove", "steps": ["c", "a"]},
            {"op": "step.add", "step": "a", "spec": {"run": "x"}},
            {"op": "step.add", "step": "d", "spec": {"run": "x"}},
        ])),
    )
    .unwrap();
    let applied = ops::apply(&case.base, &case.ops, true, &IndexMap::new(), &Open).unwrap();
    assert_eq!(
        exported(&applied),
        r#"{"steps":{"b":{"run":"x"},"a":{"run":"x"},"d":{"run":"x"}}}"#
    );
    let changes = serde_json::to_value(ops::changes(&case.base, &applied.rows)).unwrap();
    assert_eq!(
        changes,
        json!([
            {"op": "step.delete", "step": "c"},
            {"op": "step.put", "step": "a", "position": 2, "declaration": {"run": "x"}},
            {"op": "step.put", "step": "d", "position": 3, "declaration": {"run": "x"}},
        ])
    );
    assert_eq!(
        applied.added.unwrap(),
        ["a".parse().unwrap(), "d".parse().unwrap()]
    );
}

#[test]
fn order_set_renumbers_its_collection_and_a_later_add_follows_it() {
    let base = json!({"steps": {"a": {"run": "x"}, "b": {"run": "x"}, "c": {"run": "x"}}});
    let applied = apply(
        base.clone(),
        json!([
            {"op": "order.set", "collection": "steps", "ids": ["c", "a", "b"]},
            {"op": "step.add", "step": "d", "spec": {"run": "x"}},
        ]),
        true,
    )
    .unwrap();
    let positions: Vec<_> = applied
        .rows
        .steps
        .iter()
        .map(|r| (r.step.to_string(), r.position))
        .collect();
    assert_eq!(
        positions,
        [
            ("c".into(), 0),
            ("a".into(), 1),
            ("b".into(), 2),
            ("d".into(), 3)
        ]
    );
    assert_eq!(
        apply(
            base,
            json!([{"op": "order.set", "collection": "steps", "ids": ["c", "c", "z"]}]),
            true
        )
        .unwrap_err(),
        [
            "ops[0].ids[1]: c is listed twice",
            "ops[0].ids[2]: no step z",
            "ops[0].ids: a is missing",
            "ops[0].ids: b is missing",
        ]
    );
}

#[test]
fn every_refused_operation_is_named_and_a_refused_one_changes_nothing() {
    let refused = apply(
        json!({"steps": {"release": {"run": "x"}}}),
        json!([
            {"op": "step.add", "step": "release", "spec": {"run": "x"}},
            {"op": "step.update", "step": "relase", "changes": {"priority": 1}},
            {"op": "input.remove", "name": "repo"},
            {"op": "output.remove", "name": "notes"},
            {"op": "step.remove", "steps": ["release", "gone"]},
            {"op": "edge.add", "step": "unit:nowhere", "after": ["!release", "x/y?", "unit:gone"]},
            {"op": "unit.update", "unit": "release", "changes": {"other": {"priority": 1}}},
            {"op": "unit.remove", "unit": "gone"},
            {"op": "unit.add", "recipe": "absent", "unit": "w"},
            {"op": "unit.add", "recipe": "lane", "unit": "w", "params": {"unit": "v"}},
        ]),
        true,
    )
    .unwrap_err();
    assert_eq!(
        refused,
        [
            "ops[0].step: step release already exists",
            "ops[1].step: no step relase",
            "ops[2].name: no input repo",
            "ops[3].name: no output notes",
            "ops[4].steps[1]: no step gone",
            "ops[5].step: no unit nowhere",
            "ops[5].after[0]: ! is only allowed on boolean refs, not step entries",
            "ops[5].after[1]: ? is only allowed on step and unit entries, not refs",
            "ops[5].after[2]: no unit gone",
            "ops[6].changes.other: not a step of unit release",
            "ops[7].unit: no unit gone",
            "ops[8].recipe: no recipe absent",
            "ops[9].params.unit: must match the requested unit",
        ]
    );
    // the refused unit.add added nothing, so the edge.add after it has no unit to gate
    assert_eq!(
        apply(
            json!({"steps": {}}),
            json!([
                {"op": "unit.add", "recipe": "lane", "unit": "w"},
                {"op": "edge.add", "step": "unit:w", "after": ["unit:w"]},
            ]),
            true,
        )
        .unwrap_err(),
        [
            "ops[0].params.ticket: required",
            "ops[1].step: no unit w",
            "ops[1].after[0]: no unit w",
        ]
    );
}

#[test]
fn an_edge_on_a_unit_gates_its_entry_steps_and_removing_the_last_entry_drops_after() {
    let base = json!({"steps": {
        "a": {"run": "x", "tags": ["unit:u"]},
        "b": {"run": "x", "tags": ["unit:u"], "after": ["a"]},
        "c": {"run": "x", "tags": ["unit:u"]},
        "z": {"run": "x"},
    }});
    let applied = apply(
        base.clone(),
        json!([{"op": "edge.add", "step": "unit:u", "after": ["z", "z"]}]),
        true,
    )
    .unwrap();
    assert_eq!(
        exported(&applied),
        r#"{"steps":{"a":{"run":"x","tags":["unit:u"],"after":["z"]},"b":{"run":"x","tags":["unit:u"],"after":["a"]},"c":{"run":"x","tags":["unit:u"],"after":["z"]},"z":{"run":"x"}}}"#
    );
    let applied = apply(
        base,
        json!([{"op": "edge.remove", "step": "b", "after": ["a"]}]),
        true,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(ops::document_of(&applied.rows)).unwrap()["steps"]["b"],
        json!({"run": "x", "tags": ["unit:u"]})
    );
}

#[test]
fn a_unit_add_gates_its_entries_and_start_false_pauses_every_added_step() {
    let fixture = Fixture::new();
    let mut case = EditCase::new(
        &map(json!({"steps": {"s0": {"run": "t.flag"}}})),
        op_list(json!([
            {"op": "unit.add", "recipe": "lane", "unit": "w", "params": {"ticket": "FIG-1"},
                "after": {"*": ["s0"]}, "tags": ["wave-2"]},
            {"op": "step.add", "step": "n", "spec": {"run": "t.flag", "paused": "held"}},
        ])),
    )
    .unwrap();
    case.start = false;
    let Verdict::Accepted(accepted) = evaluate(&case, &fixture.context()).unwrap() else {
        panic!("accepted");
    };
    let document = serde_json::to_value(&accepted.document).unwrap();
    assert_eq!(
        document["steps"]["w-fork"],
        json!({"run": "t.text", "in": {"text": {"default": "FIG-1"}},
            "tags": ["unit:w", "wave-2"], "after": ["s0"], "paused": true})
    );
    assert_eq!(
        document["steps"]["w-work"]["after"],
        json!(["w-fork"]),
        "only the entry step is gated"
    );
    assert_eq!(document["steps"]["n"]["paused"], json!("held"));
    assert_eq!(
        accepted.steps.as_ref().unwrap(),
        &["w-fork", "w-work", "w-land", "n"]
            .map(|s| s.parse().unwrap())
            .to_vec()
    );
    assert_eq!(
        accepted.impact.would_start,
        Vec::<sluice_model::ids::StepId>::new(),
        "every added step is paused"
    );
}

#[test]
fn the_contract_edit_example_is_accepted_with_its_change_kinds() {
    let fixture = Fixture::new();
    let base = json!({
        "inputs": {"repo": "string"},
        "steps": {
            "work": {"run": "t.text", "in": {"text": {"default": "go"}}},
            "release": {"run": "t.flag", "tags": ["unit:release"], "after": ["work"]},
        }
    });
    let request: Value = serde_json::from_str(include_str!(
        "../../sluice-model/tests/fixtures/plan_rows/plan_edit.request.json"
    ))
    .unwrap();
    let mut ops = op_list(request["ops"].clone());
    // the example's review runs agent.review on work/final; here it is t.review on work/text
    if let PlanOp::StepAdd { spec, .. } = &mut ops[0] {
        *spec = map(json!({"run": "t.review", "after": ["work"],
            "in": {"spec": {"source": "work/text"}}}));
    }
    let case = EditCase::new(&map(base), ops).unwrap();
    let Verdict::Accepted(accepted) = evaluate(&case, &fixture.context()).unwrap() else {
        panic!("accepted");
    };
    let kinds: Vec<_> = serde_json::to_value(&accepted.changes)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|c| format!("{} {}", c["op"], c.get("step").or(c.get("name")).unwrap()))
        .collect();
    assert_eq!(
        kinds,
        [
            r#""input.put" "reviewer""#,
            r#""step.put" "release""#,
            r#""step.put" "review""#
        ]
    );
    assert_eq!(
        accepted.steps.as_ref().unwrap(),
        &["review".parse().unwrap()].to_vec()
    );
    assert_eq!(
        serde_json::to_value(&accepted.document).unwrap()["steps"]["release"]["after"],
        json!(["work", "review"])
    );
}

#[test]
fn an_update_that_leaves_the_data_equal_changes_nothing_and_keeps_the_stored_bytes() {
    let base =
        json!({"steps": {"a": {"run": "x", "in": {"p": {"default": 1}, "q": {"default": -0.0}}}}});
    let case = EditCase::new(
        &map(base.clone()),
        op_list(json!([
            {"op": "step.update", "step": "a", "changes": {"run": null}},
            {"op": "step.update", "step": "a",
                "changes": {"in": {"q": {"default": -0.0}, "p": {"default": 1}}, "run": "x"}},
        ])),
    )
    .unwrap();
    let applied = ops::apply(&case.base, &case.ops, true, &IndexMap::new(), &Open).unwrap();
    assert_eq!(ops::changes(&case.base, &applied.rows), []);
    assert_eq!(exported(&applied), serde_json::to_string(&base).unwrap());
    // a signed zero, or an int that becomes a float, is a change of data
    for value in [json!(0.0), json!(1.0)] {
        let key = if value == json!(0.0) { "q" } else { "p" };
        let mut bindings = json!({"p": {"default": 1}, "q": {"default": -0.0}});
        bindings[key]["default"] = value;
        let applied = apply(
            base.clone(),
            json!([{"op": "step.update", "step": "a", "changes": {"in": bindings}}]),
            true,
        )
        .unwrap();
        let base_rows = EditCase::new(&map(base.clone()), vec![]).unwrap().base;
        assert_eq!(ops::changes(&base_rows, &applied.rows).len(), 1, "{key}");
    }
}
