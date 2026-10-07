use indexmap::IndexMap;
use proptest::prelude::*;
use serde_json::{Value, json};
use sluice_model::{
    commands::{PatchOperation, StepStatus},
    gates::{
        Gate, GateDecision, StateSnapshot, StepState, ValueRef, evaluate_gate, evaluate_step,
        readiness, reconcile, resolve_reference,
    },
    ids::{StepId, UnitName},
    plan::{
        FnSignature, Pause, Plan, ResourceLimit, inputs_hash, topological_order,
        validate_changed_needs,
    },
    rpc::{JsonMap, JsonValue, decode_json},
    types::{BoundValue, Type},
    units::retry_walk,
};

fn id(name: &str) -> StepId {
    name.parse().unwrap()
}
fn unit(name: &str) -> UnitName {
    name.parse().unwrap()
}
fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn signatures() -> IndexMap<String, FnSignature> {
    let signature = |inputs: Value, outputs: Value, open| FnSignature {
        inputs: inputs
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, form)| (name.clone(), Type::parse(form).unwrap()))
            .collect(),
        outputs: outputs
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, form)| (name.clone(), Type::parse(form).unwrap()))
            .collect(),
        open,
        ..FnSignature::default()
    };
    IndexMap::from([
        (
            "test.add".into(),
            signature(json!({"a":"int","b":"int"}), json!({"sum":"int"}), false),
        ),
        (
            "test.split".into(),
            signature(json!({"text":"string"}), json!({"parts":"string[]"}), false),
        ),
        (
            "test.window".into(),
            signature(
                json!({"seconds":"float","tag":"string?"}),
                json!({"end":"float", "start":"float"}),
                false,
            ),
        ),
        (
            "test.open".into(),
            signature(
                json!({"attempts":"Any[]?"}),
                json!({"results":"int[]"}),
                true,
            ),
        ),
        (
            "core.echo".into(),
            signature(json!({"value":"Any"}), json!({"value":"Any"}), false),
        ),
        (
            "core.collect".into(),
            signature(json!({"items":"Any[]"}), json!({"items":"Any[]"}), false),
        ),
        (
            "core.external".into(),
            signature(json!({}), json!({}), true),
        ),
    ])
}
fn parse(value: Value) -> Plan {
    Plan::parse(&map(value), &signatures()).unwrap()
}
fn external() -> Value {
    json!({"run":"core.external", "outputs":{"flag":"Any", "n":"int", "optional":"boolean?"}})
}
fn worker(after: Value) -> Value {
    json!({"run":"test.add", "in":{"a":{"default":1}, "b":{"default":1}}, "after":after})
}
fn state(status: StepStatus, outputs: Value) -> StepState {
    StepState {
        status,
        outputs: map(outputs),
        inputs_hash: Some(sluice_model::hash::InputsHash::of(&JsonMap::default()).unwrap()),
        ..StepState::default()
    }
}
fn upstream(status: StepStatus, value: Value) -> StateSnapshot {
    StateSnapshot {
        steps: IndexMap::from([(id("up"), state(status, json!({"flag":value,"n":2})))]),
        ..StateSnapshot::default()
    }
}
fn gate_plan(entries: Value) -> Plan {
    let mut up = external();
    up["tags"] = json!(["unit:delivery", "exit"]);
    parse(
        json!({"inputs":{"required":"boolean","optional":"boolean?","any":"Any"}, "steps":{"up":up,"down":worker(entries)}}),
    )
}
fn decision(entries: Value, snapshot: &StateSnapshot) -> GateDecision {
    let plan = gate_plan(entries);
    evaluate_step(&plan, snapshot, &plan.steps()[&id("down")])
}
fn reasons(decision: GateDecision) -> Vec<String> {
    match decision {
        GateDecision::Skip(reasons) => reasons.iter().map(ToString::to_string).collect(),
        other => panic!("expected skip, got {other:?}"),
    }
}

#[test]
fn every_gate_form_over_every_upstream_status() {
    let statuses = [
        StepStatus::Pending,
        StepStatus::Running,
        StepStatus::Succeeded,
        StepStatus::Failed,
        StepStatus::Stale,
        StepStatus::Skipped,
    ];
    for entry in [
        "up",
        "up?",
        "up/flag",
        "!up/flag",
        "unit:delivery",
        "unit:delivery?",
    ] {
        for status in statuses.clone() {
            for value in [json!(true), json!(false), Value::Null, json!(3)] {
                let got = decision(json!([entry]), &upstream(status.clone(), value.clone()));
                let expected = match status {
                    StepStatus::Skipped if entry.ends_with('?') => "ready",
                    StepStatus::Skipped => "skip",
                    StepStatus::Succeeded if entry.contains('/') => {
                        if value.is_null() {
                            "skip"
                        } else if let Some(value) = value.as_bool() {
                            if value != entry.starts_with('!') {
                                "ready"
                            } else {
                                "skip"
                            }
                        } else {
                            "invalid"
                        }
                    }
                    StepStatus::Succeeded => "ready",
                    _ => "wait",
                };
                let actual = match got {
                    GateDecision::Ready => "ready",
                    GateDecision::Wait(_) => "wait",
                    GateDecision::Skip(_) => "skip",
                    GateDecision::Invalid(_) => "invalid",
                };
                assert_eq!(
                    actual, expected,
                    "entry={entry}, status={status:?}, value={value}"
                );
            }
        }
    }
}

#[test]
fn input_gates_distinguish_missing_null_and_strict_booleans() {
    let plan = gate_plan(json!([]));
    let mut snapshot = StateSnapshot::default();
    assert!(matches!(
        decision(json!(["required"]), &snapshot),
        GateDecision::Wait(_)
    ));
    for entry in ["optional", "!optional"] {
        assert_eq!(
            reasons(decision(json!([entry]), &snapshot)),
            ["optional is null"]
        );
    }
    for entry in ["required", "!required"] {
        for value in [
            json!(true),
            json!(false),
            Value::Null,
            json!(0),
            json!(1),
            json!("true"),
            json!([]),
            json!({}),
        ] {
            snapshot.inputs = map(json!({"required":value}));
            let got = decision(json!([entry]), &snapshot);
            if value.is_null() {
                assert_eq!(reasons(got), ["required is null"]);
            } else if let Some(value) = value.as_bool() {
                assert_eq!(got == GateDecision::Ready, value != entry.starts_with('!'));
            } else {
                assert!(matches!(got, GateDecision::Invalid(_)), "{value}");
            }
        }
    }
    snapshot.inputs = map(json!({"required":3}));
    let errors = plan.validate_input_values(&snapshot.inputs).unwrap_err();
    assert_eq!(errors[0].path, "inputs.required");
}

#[test]
fn precedence_is_invalid_then_skip_then_wait_and_preserves_diagnostics() {
    let mut snapshot = upstream(StepStatus::Succeeded, json!(3));
    snapshot.inputs = map(json!({"any": "wrong"}));
    let GateDecision::Invalid(errors) =
        decision(json!(["optional", "required", "up/flag", "any"]), &snapshot)
    else {
        panic!()
    };
    assert_eq!(errors.len(), 2);
    assert_eq!(errors[0].to_string(), "after: up/flag is 3, not a boolean");
    assert_eq!(
        errors[1].to_string(),
        "after: any is \"wrong\", not a boolean"
    );
    assert!(matches!(
        decision(json!(["required", "optional"]), &snapshot),
        GateDecision::Skip(_)
    ));
    assert!(matches!(
        decision(json!(["up", "required"]), &snapshot),
        GateDecision::Wait(_)
    ));
    assert_eq!(decision(json!(["up"]), &snapshot), GateDecision::Ready);
}

#[test]
fn pauses_precede_skip_and_invalid_and_leave_running_work_alone() {
    let plan = gate_plan(json!(["optional", "up/flag"]));
    let snapshot = upstream(StepStatus::Succeeded, json!(3));
    let paused = plan
        .patch(
            &decode_json::<Vec<PatchOperation>>(
                br#"[{"op":"add","path":"/steps/down/paused","value":"review"}]"#,
            )
            .unwrap(),
            &signatures(),
        )
        .unwrap();
    assert_eq!(
        evaluate_step(&paused, &snapshot, &paused.steps()[&id("down")]),
        GateDecision::Wait(vec!["paused: review".into()])
    );
    assert_eq!(
        reconcile(&paused, &snapshot).status(&id("down")),
        StepStatus::Pending
    );
    let mut held = snapshot.clone();
    held.paused = Pause::Yes;
    assert_eq!(
        evaluate_step(&plan, &held, &plan.steps()[&id("down")]),
        GateDecision::Wait(vec!["paused".into()])
    );
    held.steps
        .insert(id("down"), state(StepStatus::Running, json!({})));
    assert_eq!(
        reconcile(&plan, &held).status(&id("down")),
        StepStatus::Running
    );
}

#[test]
fn handoffs_share_skip_wait_rules_and_skip_propagates_in_dependency_order() {
    let plan = parse(json!({"steps":{
        "a": worker(json!(["go"])),
        "b":{"run":"core.echo","in":{"value":{"source":"a/sum"}}},
        "c":worker(json!(["b"])),
        "cleanup":worker(json!(["c?"]))},"inputs":{"go":"boolean"}}));
    let snapshot = StateSnapshot {
        inputs: map(json!({"go":false})),
        ..StateSnapshot::default()
    };
    let settled = reconcile(&plan, &snapshot);
    for name in ["a", "b", "c"] {
        assert_eq!(settled.status(&id(name)), StepStatus::Skipped);
    }
    assert_eq!(
        evaluate_step(&plan, &settled, &plan.steps()[&id("cleanup")]),
        GateDecision::Ready
    );
    let mut yes = settled;
    yes.inputs = map(json!({"go":true}));
    let next = reconcile(&plan, &yes);
    for name in ["a", "b", "c"] {
        assert_eq!(next.status(&id(name)), StepStatus::Pending);
    }
    assert!(matches!(
        readiness(&plan, &next)[&id("b")],
        GateDecision::Wait(_)
    ));
}

#[test]
fn skipped_steps_update_their_reasons_and_gate_edits_rearm_them() {
    let plan = gate_plan(json!(["up/flag"]));
    let skipped = reconcile(&plan, &upstream(StepStatus::Succeeded, json!(false)));
    assert_eq!(
        skipped.steps[&id("down")].skipped[0].to_string(),
        "up/flag is false"
    );
    let mut null = skipped.clone();
    null.steps.get_mut(&id("up")).unwrap().outputs = map(json!({"flag":null}));
    let null = reconcile(&plan, &null);
    assert_eq!(
        null.steps[&id("down")].skipped[0].to_string(),
        "up/flag is null"
    );
    let changed = plan
        .patch(
            &decode_json::<Vec<PatchOperation>>(br#"[{"op":"remove","path":"/steps/down/after"}]"#)
                .unwrap(),
            &signatures(),
        )
        .unwrap();
    assert_eq!(
        reconcile(&changed, &skipped).status(&id("down")),
        StepStatus::Pending
    );
}

#[test]
fn available_missing_optional_outputs_and_null_ancestors_are_null() {
    let plan = parse(
        json!({"steps":{"up":{"run":"core.external","outputs":{"record":{"type":"record","fields":{"flag":"boolean?"}},"optional":"boolean?","needed":"boolean"}},"down":worker(json!(["up/record.flag"]))}}),
    );
    let mut snapshot = upstream(StepStatus::Succeeded, json!(true));
    snapshot.steps[&id("up")].outputs = map(json!({"record":{}}));
    for reference in ["up/optional", "up/record.flag"] {
        assert!(
            matches!(resolve_reference(&plan,&snapshot,&ValueRef::parse(reference).unwrap()),BoundValue::Ready(value) if value.as_value().is_null())
        );
    }
    assert!(matches!(
        resolve_reference(&plan, &snapshot, &ValueRef::parse("up/needed").unwrap()),
        BoundValue::Waiting
    ));
}

#[test]
fn valid_plan_preserves_order_scatter_navigation_and_typed_handoffs() {
    let plan = parse(
        json!({"inputs":{"n":{"type":"int","doc":"Count"},"words":"string[]","note":"string?"},"outputs":{"total":{"source":"b/sum"},"first":{"source":"split/parts.0"}},"steps":{
        "a":{"run":"test.add","in":{"a":{"source":"n"},"b":{"default":1}},"tags":["unit:one"]},
        "b":{"run":"test.add","in":{"a":{"source":"a/sum"},"b":{"default":1}},"tags":["unit:two"]},
        "split":{"run":"test.split","in":{"text":{"default":"x y"}}},
        "each":{"run":"test.window","scatter":"tag","in":{"seconds":{"default":0},"tag":{"source":"words"}}},
        "gate":{"run":"core.collect","in":{"items":{"source":["a/sum","each/end","note"]}}},
        "first":{"run":"core.echo","in":{"value":{"source":"each/start.0"}}}}}),
    );
    assert_eq!(
        plan.inputs().keys().map(String::as_str).collect::<Vec<_>>(),
        ["n", "words", "note"]
    );
    assert_eq!(
        plan.outputs()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["total", "first"]
    );
    assert_eq!(plan.steps()[&id("b")].data_dependencies(), [id("a")]);
    assert_eq!(
        plan.steps()[&id("gate")].data_dependencies(),
        [id("a"), id("each")]
    );
    assert_eq!(
        plan.steps()[&id("each")].output_type("end"),
        Some(Type::List(Box::new(Type::Float)))
    );
    assert_eq!(plan.inputs()["n"].doc.as_deref(), Some("Count"));
    assert_ne!(
        plan.steps()[&id("a")].unit_name(),
        plan.steps()[&id("b")].unit_name()
    );
}

#[test]
fn open_fan_in_keeps_all_twenty_five_cross_unit_handoffs() {
    let lanes: Vec<String> = (0..25).map(|n| format!("lane-{n}")).collect();
    let mut steps = serde_json::Map::new();
    for lane in &lanes {
        steps.insert(
            format!("{lane}-work"),
            json!({"run":"test.open","tags":[format!("unit:{lane}"),"exit"],"in":{"attempts":{"default":[]}}}),
        );
    }
    let sources: Vec<String> = lanes
        .iter()
        .map(|lane| format!("{lane}-work/results"))
        .collect();
    steps.insert(
        "integrate-work".into(),
        json!({"run":"test.open","tags":["unit:integrate","exit"],"in":{"lanes":{"source":sources}}}),
    );
    let plan = parse(json!({"steps": steps}));
    assert_eq!(plan.units().len(), 26);
    let step = &plan.steps()[&id("integrate-work")];
    assert_eq!(step.bindings["lanes"].references().len(), 25);
    assert!(step.bindings["lanes"].references().iter().all(|reference| {
        reference
            .parts()
            .unwrap()
            .step
            .is_some_and(|source| plan.steps()[&source].unit_name() != step.unit_name())
    }));
}

#[test]
fn closed_shapes_and_value_errors_report_every_path() {
    let errors = Plan::parse(
        &map(
            json!({"label":1,"rev":2,"inputs":{"Bad":"int", "no":{"doc":"missing type"}},"steps":{
        "a":{"run":"test.add","when":"yes","in":{"a":{"default":"x"}}},
        "b":{"run":"missing.fn","bogus":1},
        "c":{"run":"test.add","in":{"a":{"source":"missing"},"b":{"default":1,"source":"x"}}}}}),
        ),
        &signatures(),
    )
    .unwrap_err();
    let paths: Vec<_> = errors.iter().map(|error| error.path.as_str()).collect();
    for path in [
        "label",
        "rev",
        "inputs.Bad",
        "inputs.no.type",
        "steps.a.when",
        "steps.a.in.a",
        "steps.a.in.b",
        "steps.b.bogus",
        "steps.b.run",
        "steps.c.in.a",
        "steps.c.in.b",
    ] {
        assert!(paths.contains(&path), "missing {path}: {errors:?}");
    }
}

#[test]
fn old_plan_validation_scenarios_and_v2_refusals() {
    let cases = vec![
        (json!({}), "steps"),
        (json!({"steps":[],"inputs":[]}), "steps"),
        (json!({"steps":{"Bad":worker(json!([]))}}), "steps.Bad"),
        (json!({"inputs":{"x":"nope"},"steps":{}}), "inputs.x"),
        (
            json!({"steps":{"a":{"run":"test.add","in":{"a":{"default":1},"b":{"default":1},"c":{"default":1}}}}}),
            "steps.a.in.c",
        ),
        (
            json!({"steps":{"a":{"run":"test.add","in":{"a":{"source":"zz/sum"},"b":{"default":1}}}}}),
            "steps.a.in.a",
        ),
        (
            json!({"steps":{"a":{"run":"test.add","in":{"a":{"source":"a/nope"},"b":{"default":1}}}}}),
            "steps.a.in.a",
        ),
        (
            json!({"steps":{"a":{"run":"test.add","in":{"a":{"source":"a/sum.x"},"b":{"default":1}}}}}),
            "steps.a.in.a",
        ),
        (
            json!({"inputs":{"s":"string"},"steps":{"a":{"run":"test.add","in":{"a":{"source":"s"},"b":{"default":1}}}}}),
            "steps.a.in.a",
        ),
        (
            json!({"inputs":{"s":"int?"},"steps":{"a":{"run":"test.add","in":{"a":{"source":"s"},"b":{"default":1}}}}}),
            "steps.a.in.a",
        ),
        (
            json!({"steps":{"a":{"run":"test.add","in":{"a":{"source":[]},"b":{"default":1}}}}}),
            "steps.a.in.a",
        ),
        (
            json!({"outputs":{"o":{"default":1}},"steps":{}}),
            "outputs.o",
        ),
        (
            json!({"outputs":{"o":{"source":"zz/x"}},"steps":{}}),
            "outputs.o",
        ),
        (
            json!({"steps":{"a":{"run":"test.window","scatter":"seconds","in":{"seconds":{"default":1}}}}}),
            "steps.a.in.seconds",
        ),
        (
            json!({"steps":{"a":{"run":"test.window","scatter":"tag","in":{"seconds":{"default":1}}}}}),
            "steps.a.scatter",
        ),
        (json!({"steps":{"owner":external()}}), "steps.owner"),
        (
            json!({"steps":{"orchestrator":external()}}),
            "steps.orchestrator",
        ),
        (
            json!({"inputs":{"a":"int"},"steps":{"a":external()}}),
            "steps.a",
        ),
        (
            json!({"steps":{"a":{"run":"core.external","tags":["unit:a","unit:b"]}}}),
            "steps.a.tags",
        ),
        (
            json!({"steps":{"a":{"run":"core.external","paused":" "}}}),
            "steps.a.paused",
        ),
        (
            json!({"steps":{"a":{"run":"core.external","priority":true}}}),
            "steps.a.priority",
        ),
        (
            json!({"steps":{"a":{"run":"core.external","needs":{"lane":true}}}}),
            "steps.a.needs.lane",
        ),
    ];
    for (value, path) in cases {
        let errors = Plan::parse(&map(value), &signatures()).unwrap_err();
        assert!(
            errors.iter().any(|error| error.path == path),
            "{path}: {errors:?}"
        );
    }
}

#[test]
fn gate_typing_refuses_question_refs_and_not_step_unit_entries() {
    for entry in [
        "up/flag?",
        "required?",
        "!up",
        "!up?",
        "!unit:delivery",
        "!unit:delivery?",
        "up/n",
        "unit:gone",
        "unit:delivery#exit",
        "up/flag??",
    ] {
        let mut doc = map(
            json!({"inputs":{"required":"boolean"},"steps":{"up":external(),"down":worker(json!([entry]))}}),
        );
        // The valid delivery unit is present, so !unit rejection is syntax, not lookup.
        let mut up = external();
        up["tags"] = json!(["unit:delivery"]);
        doc.0.insert(
            "steps".into(),
            JsonValue::try_from(json!({"up":up,"down":worker(json!([entry]))})).unwrap(),
        );
        let errors = Plan::parse(&doc, &signatures()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.path.starts_with("steps.down.after")),
            "{entry}: {errors:?}"
        );
    }
}

#[test]
fn refs_files_and_scatter_reject_malformed_shapes() {
    for text in ["a/b/c", "a..b", "a.b/c", "a/", "/a", "a/x.", "A/x", ""] {
        assert!(ValueRef::parse(text).is_err(), "{text}");
    }
    assert_eq!(
        ValueRef::parse("a/out.0.x")
            .unwrap()
            .parts()
            .unwrap()
            .fields,
        ["0", "x"]
    );
    for (binding, fragment) in [
        (json!({"file":"relative"}), ".file"),
        (json!({"file":false}), ".file"),
        (json!({"source":3}), ".source"),
        (json!({"source":[3,"bad..ref"]}), ".source["),
        (json!({"default":1,"file":"/tmp/x"}), "steps.a.in.value"),
    ] {
        let errors = Plan::parse(
            &map(json!({"steps":{"a":{"run":"core.echo","in":{"value":binding}}}})),
            &signatures(),
        )
        .unwrap_err();
        assert!(
            errors.iter().any(|error| error.path.contains(fragment)),
            "{errors:?}"
        );
    }
    let errors=Plan::parse(&map(json!({"steps":{"a":{"run":"test.add","in":{"a":{"file":"/tmp/n"},"b":{"default":1}}}}})),&signatures()).unwrap_err();
    assert!(errors[0].message.contains("file binding is a string"));
    assert!(Plan::parse_json(br#"{"steps":{},"steps":{}}"#, &signatures()).is_err());
}

fn certify(plan: &Plan, snapshot: &mut StateSnapshot) {
    for id in plan.topological_order() {
        if matches!(
            snapshot.status(id),
            StepStatus::Succeeded | StepStatus::Stale
        ) {
            let hash = inputs_hash(plan, snapshot, &plan.steps()[id]);
            snapshot.steps.get_mut(id).unwrap().inputs_hash = hash;
        }
    }
}

#[test]
fn units_are_tags_and_singletons_independent_of_cross_unit_edges() {
    let plan = parse(json!({"steps":{
        "a":{"run":"core.external","tags":["unit:lane"]},
        "loose":worker(json!(["a"])),
        "b":{"run":"core.external","tags":["unit:lane"],"after":["a"]},
        "last":worker(json!(["b","loose"]))}}));
    assert_eq!(
        plan.units()
            .keys()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["lane", "loose", "last"]
    );
    let lane = &plan.units()[&unit("lane")];
    assert_eq!(lane.steps, [id("a"), id("b")]);
    assert_eq!(lane.entries, [id("a")]);
    assert_eq!(lane.exits, [id("b")]);
    assert_eq!(plan.units()[&unit("loose")].steps, [id("loose")]);
}

#[test]
fn every_internal_relation_affects_entries_and_sink_exits() {
    for relation in ["handoff", "step", "step?", "boolean", "not"] {
        let mut a = external();
        a["tags"] = json!(["unit:lane"]);
        let mut b = worker(json!([]));
        b["tags"] = json!(["unit:lane"]);
        match relation {
            "handoff" => b["in"]["a"] = json!({"source":"a/n"}),
            "step" => b["after"] = json!(["a"]),
            "step?" => b["after"] = json!(["a?"]),
            "boolean" => b["after"] = json!(["a/flag"]),
            "not" => b["after"] = json!(["!a/flag"]),
            _ => unreachable!(),
        }
        let plan = parse(json!({"steps":{"a":a,"b":b}}));
        let lane = &plan.units()[&unit("lane")];
        assert_eq!(lane.entries, [id("a")], "{relation}");
        assert_eq!(lane.exits, [id("b")], "{relation}");
    }
}

#[test]
fn explicit_delivery_exits_do_not_include_cleanup() {
    let plan = parse(json!({"steps":{
        "a":{"run":"core.external","tags":["unit:lane","exit"]},
        "cleanup":{"run":"test.add","tags":["unit:lane"],"in":{"a":{"default":1},"b":{"default":1}},"after":["a?"]},
        "join":worker(json!(["unit:lane"]))}}));
    assert_eq!(plan.units()[&unit("lane")].exits, [id("a")]);
    let snapshot = StateSnapshot {
        steps: IndexMap::from([
            (id("a"), state(StepStatus::Succeeded, json!({}))),
            (id("cleanup"), state(StepStatus::Running, json!({}))),
        ]),
        ..StateSnapshot::default()
    };
    let lane = &plan.units()[&unit("lane")];
    assert!(lane.exit_success(&snapshot));
    assert!(!lane.settled(&plan, &snapshot));
    assert!(!lane.done(&snapshot));
    assert_eq!(
        evaluate_step(&plan, &snapshot, &plan.steps()[&id("join")]),
        GateDecision::Ready
    );
}

#[test]
fn all_skipped_is_done_but_does_not_deliver_plain_unit_gate() {
    let plan = gate_plan(json!(["unit:delivery"]));
    let snapshot = upstream(StepStatus::Skipped, json!(true));
    let unit = &plan.units()[&unit("delivery")];
    assert!(unit.done(&snapshot));
    assert!(unit.settled(&plan, &snapshot));
    assert!(!unit.exit_success(&snapshot));
    assert!(matches!(
        evaluate_step(&plan, &snapshot, &plan.steps()[&id("down")]),
        GateDecision::Skip(_)
    ));
}

#[test]
fn settlement_distinguishes_queued_paused_failed_stale_and_external() {
    let plan = parse(
        json!({"steps":{"work":worker(json!([])),"external":external(),"blocked":worker(json!(["external"]))}}),
    );
    let snapshot = StateSnapshot::default();
    assert!(!plan.units()[&unit("work")].settled(&plan, &snapshot));
    assert!(plan.units()[&unit("external")].settled(&plan, &snapshot));
    assert!(plan.units()[&unit("blocked")].settled(&plan, &snapshot));
    for status in [
        StepStatus::Failed,
        StepStatus::Stale,
        StepStatus::Skipped,
        StepStatus::Succeeded,
    ] {
        let mut snapshot = snapshot.clone();
        snapshot.steps.insert(id("work"), state(status, json!({})));
        assert!(plan.units()[&unit("work")].settled(&plan, &snapshot));
    }
    let mut queued = snapshot.clone();
    queued.steps.insert(
        id("work"),
        StepState {
            queued: vec!["lane".into()],
            ..StepState::default()
        },
    );
    assert!(!plan.units()[&unit("work")].settled(&plan, &queued));
    queued.paused = Pause::Yes;
    assert!(plan.units()[&unit("work")].settled(&plan, &queued));
    queued.steps[&id("work")].status = StepStatus::Running;
    assert!(!plan.units()[&unit("work")].settled(&plan, &queued));
}

#[test]
fn cycles_include_every_relation_and_expand_unit_entries_to_exits() {
    for entry in ["a", "a?", "a/flag", "!a/flag", "unit:lane", "unit:lane?"] {
        let mut a = external();
        a["tags"] = json!(["unit:lane", "exit"]);
        a["after"] = json!(["b"]);
        let errors = Plan::parse(
            &map(json!({"steps":{"a":a,"b":worker(json!([entry]))}})),
            &signatures(),
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("dependency cycle a -> b -> a")),
            "{entry}: {errors:?}"
        );
    }
    let errors=Plan::parse(&map(json!({"steps":{"a":{"run":"core.external","tags":["unit:lane"],"after":["unit:lane?"]}}})),&signatures()).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("own exits"))
    );
    let errors = Plan::parse(
        &map(json!({"steps":{"a":{"run":"core.external","tags":["unit:lane"]},"lane":external()}})),
        &signatures(),
    )
    .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("singleton unit name collides"))
    );
}

#[test]
fn only_data_changes_make_results_stale_and_stale_can_recover() {
    let plan = parse(json!({"inputs":{"n":"int","go":"boolean"},"steps":{
        "a":{"run":"core.echo","in":{"value":{"source":"n"}},"after":["go"]},
        "b":{"run":"core.echo","in":{"value":{"source":"a/value"}}},
        "ordered":worker(json!(["a"]))}}));
    let mut snapshot = StateSnapshot {
        inputs: map(json!({"n":2,"go":true})),
        steps: IndexMap::from([
            (id("a"), state(StepStatus::Succeeded, json!({"value":2}))),
            (id("b"), state(StepStatus::Succeeded, json!({"value":2}))),
            (
                id("ordered"),
                state(StepStatus::Succeeded, json!({"sum":2})),
            ),
        ]),
        ..StateSnapshot::default()
    };
    certify(&plan, &mut snapshot);
    snapshot.inputs = map(json!({"n":2,"go":false}));
    let same = reconcile(&plan, &snapshot);
    assert!(
        same.steps
            .values()
            .all(|entry| entry.status == StepStatus::Succeeded)
    );
    snapshot.inputs = map(json!({"n":3,"go":false}));
    let changed = reconcile(&plan, &snapshot);
    assert_eq!(changed.status(&id("a")), StepStatus::Stale);
    assert_eq!(changed.status(&id("b")), StepStatus::Stale);
    assert_eq!(changed.status(&id("ordered")), StepStatus::Succeeded);
    let mut restored = changed;
    restored.inputs = map(json!({"n":2,"go":false}));
    let restored = reconcile(&plan, &restored);
    assert!(
        restored
            .steps
            .values()
            .all(|entry| entry.status == StepStatus::Succeeded)
    );
    let pending = retry_walk(&plan, &same, &[id("a")]).unwrap().apply(&same);
    assert_eq!(
        reconcile(&plan, &pending).status(&id("b")),
        StepStatus::Succeeded
    );
}

#[test]
fn file_path_and_bound_values_hash_but_fn_gates_and_unbound_optionals_do_not() {
    let plan = parse(
        json!({"inputs":{"go":"boolean"},"steps":{"a":{"run":"test.open","in":{"brief":{"file":"/abs/brief.md"}},"after":["go"]}}}),
    );
    let mut snapshot = StateSnapshot {
        inputs: map(json!({"go":true})),
        steps: IndexMap::from([(id("a"), state(StepStatus::Succeeded, json!({})))]),
        ..StateSnapshot::default()
    };
    certify(&plan, &mut snapshot);
    let changed_gate = plan
        .patch(
            &decode_json::<Vec<PatchOperation>>(
                br#"[{"op":"replace","path":"/steps/a/after","value":["!go"]}]"#,
            )
            .unwrap(),
            &signatures(),
        )
        .unwrap();
    assert_eq!(
        reconcile(&changed_gate, &snapshot).status(&id("a")),
        StepStatus::Succeeded
    );
    let mut registry = signatures();
    registry
        .get_mut("test.open")
        .unwrap()
        .inputs
        .insert("optional".into(), Type::Optional(Box::new(Type::String)));
    let new_optional = Plan::parse(plan.document(), &registry).unwrap();
    assert_eq!(
        reconcile(&new_optional, &snapshot).status(&id("a")),
        StepStatus::Succeeded
    );
    let path = plan
        .patch(
            &decode_json::<Vec<PatchOperation>>(
                br#"[{"op":"replace","path":"/steps/a/in/brief/file","value":"/abs/new.md"}]"#,
            )
            .unwrap(),
            &registry,
        )
        .unwrap();
    assert_eq!(
        reconcile(&path, &snapshot).status(&id("a")),
        StepStatus::Stale
    );
    let bound = plan
        .patch(
            &decode_json::<Vec<PatchOperation>>(
                br#"[{"op":"add","path":"/steps/a/in/optional","value":{"default":null}}]"#,
            )
            .unwrap(),
            &registry,
        )
        .unwrap();
    assert_eq!(
        reconcile(&bound, &snapshot).status(&id("a")),
        StepStatus::Stale
    );
}

#[test]
fn resource_validation_checks_only_changed_needs_against_declarations() {
    let plan = parse(json!({"steps":{"a":{"run":"test.open","needs":{"lane":2,"dynamic":9}}}}));
    let limits = IndexMap::from([
        ("lane".into(), ResourceLimit::Fixed(2)),
        ("dynamic".into(), ResourceLimit::Dynamic),
    ]);
    assert!(validate_changed_needs(None, &plan, &limits).is_ok());
    let lowered = IndexMap::from([("lane".into(), ResourceLimit::Fixed(1))]);
    assert_eq!(
        validate_changed_needs(None, &plan, &lowered)
            .unwrap_err()
            .len(),
        2
    );
    assert!(validate_changed_needs(Some(&plan), &plan, &lowered).is_ok());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    #[test]
    fn gate_precedence_is_independent_of_entry_permutation(status in 0usize..6, value in prop_oneof![Just(Value::Null),any::<bool>().prop_map(|value|json!(value)),(-8i64..8).prop_map(|value|json!(value))],reverse in any::<bool>()) {
        let status=[StepStatus::Pending,StepStatus::Running,StepStatus::Succeeded,StepStatus::Failed,StepStatus::Stale,StepStatus::Skipped][status].clone();
        let snapshot=upstream(status,value);
        let entries=if reverse {json!(["required","optional","up/flag","up?"])} else {json!(["up?","up/flag","optional","required"])};
        let decision=decision(entries,&snapshot);
        // Optional null always requests skip; a known nonboolean still wins.
        if snapshot.status(&id("up"))==StepStatus::Succeeded && snapshot.steps[&id("up")].outputs.0["flag"].as_value().is_number() {prop_assert!(matches!(decision,GateDecision::Invalid(_)));}
        else {prop_assert!(matches!(decision,GateDecision::Skip(_)));}
    }
    #[test]
    fn bounded_graph_validation_agrees_with_independent_kahn_oracle(n in 1usize..9,edges in prop::collection::vec((0usize..8,0usize..8),0..40)) {
        let mut graph:IndexMap<StepId,Vec<StepId>>=(0..n).map(|i|(id(&format!("s{i}")),vec![])).collect();
        for (a,b) in edges {if a<n && b<n && !graph[&id(&format!("s{a}"))].contains(&id(&format!("s{b}"))) {graph.get_mut(&id(&format!("s{a}"))).unwrap().push(id(&format!("s{b}")));}}
        let mut remaining=graph.clone();
        loop {
            let roots:Vec<_>=remaining.iter().filter(|(_,deps)|deps.is_empty()).map(|(id,_)|id.clone()).collect();
            if roots.is_empty() {break;}
            remaining.retain(|id,_|!roots.contains(id));for deps in remaining.values_mut() {deps.retain(|id|!roots.contains(id));}
        }
        let result=topological_order(&graph);prop_assert_eq!(result.is_ok(),remaining.is_empty());
        if let Ok(order)=result {for (id,deps) in &graph {for dep in deps {prop_assert!(order.iter().position(|id|id==dep)<order.iter().position(|other|other==id));}}}
        let steps:serde_json::Map<_,_>=graph.iter().map(|(id,deps)|(id.to_string(),worker(json!(deps)))).collect();
        prop_assert_eq!(Plan::parse(&map(json!({"steps":steps})),&signatures()).is_ok(),remaining.is_empty());
    }
}

#[test]
fn missing_required_any_waits_and_invalid_public_gate_references_fail() {
    let plan = gate_plan(json!(["any"]));
    assert!(matches!(
        evaluate_step(&plan, &StateSnapshot::default(), &plan.steps()[&id("down")]),
        GateDecision::Wait(_)
    ));
    assert!(matches!(
        evaluate_gate(
            &plan,
            &StateSnapshot::default(),
            &Gate::Step {
                id: id("gone"),
                accept_skip: true
            }
        ),
        GateDecision::Invalid(_)
    ));
    assert!(matches!(
        evaluate_gate(
            &plan,
            &StateSnapshot::default(),
            &Gate::Bool {
                reference: ValueRef("gone/value".into()),
                negate: false
            }
        ),
        GateDecision::Invalid(_)
    ));
}

#[test]
fn handoff_cycle_and_navigated_gate_cycle_are_rejected() {
    let cases = [
        json!({"steps":{
        "a":{"run":"test.add","in":{"a":{"source":"b/sum"},"b":{"default":1}}},
        "b":{"run":"test.add","in":{"a":{"source":"a/sum"},"b":{"default":1}}}}}),
        json!({"steps":{
            "a":{"run":"core.external","outputs":{"record":{"type":"record","fields":{"ok":"boolean"}}},"after":["b"]},
            "b":worker(json!(["a/record.ok"]))}}),
    ];
    for value in cases {
        let errors = Plan::parse(&map(value), &signatures()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("dependency cycle"))
        );
    }
}

#[test]
fn patch_keeps_surviving_maps_order_and_replacement_subtree_order() {
    let plan = parse(
        json!({"inputs":{"first":"string","second":"string","third":"string"},"outputs":{"one":{"source":"b/results"},"two":{"source":"c/results"}},"steps":{
        "a":{"run":"test.open","in":{"one":{"default":1},"two":{"default":2},"three":{"default":3}}},
        "b":{"run":"test.open"},"c":{"run":"test.open"}}}),
    );
    let ops:Vec<PatchOperation>=decode_json(br#"[{"op":"remove","path":"/inputs/first"},{"op":"remove","path":"/outputs/one"},{"op":"remove","path":"/steps/a/in/one"},{"op":"move","from":"/steps/b","path":"/steps/d"},{"op":"replace","path":"/steps/c","value":{"run":"test.open","in":{"z":{"default":0},"a":{"default":1}}}}]"#).unwrap();
    let after = plan.patch(&ops, &signatures()).unwrap();
    assert_eq!(
        after
            .inputs()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["second", "third"]
    );
    assert_eq!(
        after
            .outputs()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["two"]
    );
    assert_eq!(
        after
            .steps()
            .keys()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["a", "c", "d"]
    );
    assert_eq!(
        after.steps()[&id("a")]
            .bindings
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["two", "three"]
    );
    assert_eq!(
        after.steps()[&id("c")]
            .bindings
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["z", "a"]
    );
}

#[test]
fn paused_skipped_steps_keep_their_reason_until_unpaused() {
    let plan = gate_plan(json!(["optional"]));
    let mut snapshot = reconcile(&plan, &StateSnapshot::default());
    snapshot.inputs = map(json!({"optional":true}));
    snapshot.paused = Pause::Yes;
    let held = reconcile(&plan, &snapshot);
    assert_eq!(held.status(&id("down")), StepStatus::Skipped);
    assert_eq!(
        held.steps[&id("down")].skipped[0].to_string(),
        "optional is null"
    );
    let mut resumed = held;
    resumed.paused = Pause::No;
    assert_eq!(
        reconcile(&plan, &resumed).status(&id("down")),
        StepStatus::Pending
    );
}

#[test]
fn public_bool_gate_refuses_nonboolean_declared_type_before_waiting() {
    let plan = gate_plan(json!([]));
    assert!(matches!(
        evaluate_gate(
            &plan,
            &StateSnapshot::default(),
            &Gate::Bool {
                reference: ValueRef::parse("up/n").unwrap(),
                negate: false
            }
        ),
        GateDecision::Invalid(_)
    ));
}
