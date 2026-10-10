mod support;

use indexmap::IndexMap;
use proptest::prelude::*;
use serde_json::{Value, json};
use sluice_model::{
    gates::ValueRef,
    ids::StepId,
    plan::{FnSignature, topological_order},
    rpc::{JsonMap, decode_json},
    types::Type,
};
use support::compile;

fn id(name: &str) -> StepId {
    name.parse().unwrap()
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

fn external() -> Value {
    json!({"run":"core.external", "outputs":{"flag":"Any", "n":"int", "optional":"boolean?"}})
}
fn worker(after: Value) -> Value {
    json!({"run":"test.add", "in":{"a":{"default":1}, "b":{"default":1}}, "after":after})
}

#[test]
fn closed_shapes_and_value_errors_report_every_path() {
    let errors = compile(
        json!({"inputs":{"Bad":"int", "no":{"doc":"missing type"}},"steps":{
        "a":{"run":"test.add","when":"yes","in":{"a":{"default":"x"}}},
        "b":{"run":"missing.fn","bogus":1},
        "c":{"run":"test.add","in":{"a":{"source":"missing"},"b":{"default":1,"source":"x"}}}}}),
        &signatures(),
    )
    .unwrap_err();
    let paths: Vec<_> = errors.iter().map(|error| error.path.as_str()).collect();
    for path in [
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
        // The valid delivery unit is present, so !unit rejection is syntax, not lookup.
        let mut up = external();
        up["tags"] = json!(["unit:delivery"]);
        let doc = json!({"inputs":{"required":"boolean"},
            "steps":{"up":up,"down":worker(json!([entry]))}});
        let errors = compile(doc, &signatures()).unwrap_err();
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
        let errors = compile(
            json!({"steps":{"a":{"run":"core.echo","in":{"value":binding}}}}),
            &signatures(),
        )
        .unwrap_err();
        assert!(
            errors.iter().any(|error| error.path.contains(fragment)),
            "{errors:?}"
        );
    }
    let errors = compile(
        json!({"steps":{"a":{"run":"test.add","in":{"a":{"file":"/tmp/n"},"b":{"default":1}}}}}),
        &signatures(),
    )
    .unwrap_err();
    assert!(errors[0].message.contains("file binding is a string"));
    assert!(decode_json::<JsonMap>(br#"{"steps":{},"steps":{}}"#).is_err());
}

#[test]
fn cycles_include_every_relation_and_expand_unit_entries_to_exits() {
    for entry in ["a", "a?", "a/flag", "!a/flag", "unit:lane", "unit:lane?"] {
        let mut a = external();
        a["tags"] = json!(["unit:lane", "exit"]);
        a["after"] = json!(["b"]);
        let errors = compile(
            json!({"steps":{"a":a,"b":worker(json!([entry]))}}),
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
    let errors = compile(
        json!({"steps":{"a":{"run":"core.external","tags":["unit:lane"],"after":["unit:lane?"]}}}),
        &signatures(),
    )
    .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("own exits"))
    );
    let errors = compile(
        json!({"steps":{"a":{"run":"core.external","tags":["unit:lane"]},"lane":external()}}),
        &signatures(),
    )
    .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("singleton unit name collides"))
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

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
        prop_assert_eq!(compile(json!({"steps":steps}),&signatures()).is_ok(),remaining.is_empty());
    }
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
        let errors = compile(value, &signatures()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("dependency cycle"))
        );
    }
}

/// A bare gate name that is not a step reads as a plan input: a boolean gate on the input, no
/// dependency, and a `plan_refs` row whose source is that input (`plan_index::step_index`).
#[test]
fn a_bare_gate_name_that_is_no_step_is_an_input_ref() {
    use sluice_model::{gates::Gate, plan::step_index, plan_rows::SourceKind};
    let plan = compile(
        json!({"inputs":{"go":"boolean"},"steps":{"a":worker(json!(["go"]))}}),
        &signatures(),
    )
    .unwrap();
    let step = &plan.steps()[&id("a")];
    assert!(matches!(
        &step.after[..],
        [Gate::Bool { reference, negate: false }] if reference.0 == "go"
    ));
    assert!(plan.dependencies(&id("a")).is_empty());
    let rows = step_index(&id("a"), &step.declaration, &|name| name == "a");
    assert_eq!(rows.references.len(), 1);
    assert_eq!(rows.references[0].source_kind, SourceKind::Input);
    assert_eq!(rows.references[0].source_id, "go");
}
