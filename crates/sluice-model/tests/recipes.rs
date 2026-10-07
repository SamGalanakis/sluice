use serde_json::{Value, json};
use sluice_model::{
    recipe::{Recipe, catalog},
    rpc::{JsonMap, decode_json},
};

fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn recipe(steps: Value, params: Value) -> Recipe {
    Recipe::parse("r", &json!({"name":"r","params":params,"steps":steps})).unwrap()
}

fn value(map: &JsonMap) -> Value {
    serde_json::to_value(map).unwrap()
}

#[test]
fn substitution_in_ids_nested_strings_and_object_keys() {
    let r = recipe(
        json!({"{unit}-work":{"doc":"Work {unit} {n} {flag}", "in":{"{key}":{"default":["see {unit}",{"deep":"{unit}!"}]}}}}),
        json!({"n":"int","flag":"boolean","key":"string"}),
    );
    assert_eq!(
        value(
            &r.substitute(&map(json!({"unit":"u1","n":3,"flag":false,"key":"extra"})))
                .unwrap()
        ),
        json!({"u1-work":{"doc":"Work u1 3 false","in":{"extra":{"default":["see u1",{"deep":"u1!"}]}}}})
    );
}
#[test]
fn exact_placeholders_keep_types_optional_params_become_null() {
    let r = recipe(
        json!({"a":{"in":{"n":{"default":"{n}"},"tags":"{tags}","o":"{opt}","text":"{n}{n}","object":"{obj}"}}}),
        json!({"n":"int","tags":"string[]","opt":"string?","obj":"Any"}),
    );
    assert_eq!(
        value(
            &r.substitute(&map(
                json!({"unit":"u","n":7,"tags":["x","y"],"obj":{"a":true}})
            ))
            .unwrap()
        )["a"]["in"],
        json!({"n":{"default":7},"tags":["x","y"],"o":null,"text":"77","object":{"a":true}})
    );
}
#[test]
fn literal_braces_and_unicode_are_preserved() {
    let r = recipe(
        json!({"a":{"doc":"é{{unit}} is {unit}; {{not a param}} and }}{{"}}),
        json!({}),
    );
    assert_eq!(
        value(&r.substitute(&map(json!({"unit":"u"}))).unwrap())["a"]["doc"],
        "é{unit} is u; {not a param} and }{"
    );
}
#[test]
fn unknown_params_and_lone_braces_name_each_location() {
    let errors = Recipe::parse("r", &json!({"name":"r","steps":{"a-{unit}":{"doc":"{nope}","in":{"{unknown}":"a { b", "x":"oops }"}}}})).unwrap_err();
    let errors: Vec<_> = errors.iter().map(ToString::to_string).collect();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("steps.a-{unit}.doc: unknown param {nope}"))
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("(key): unknown param {unknown}"))
    );
    assert_eq!(errors.iter().filter(|e| e.contains("lone")).count(), 2);
}
#[test]
fn params_collect_unknown_missing_bad_unit_and_type_errors() {
    let r = recipe(
        json!({"a":{}}),
        json!({"n":"int","kind":{"type":"enum","symbols":["a","b"]}}),
    );
    let errors = r
        .substitute(&map(json!({"unit":"U 1","n":"three","kind":"c","extra":1})))
        .unwrap_err();
    assert_eq!(errors.len(), 4);
    assert_eq!(
        errors.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        ["params.extra", "params.unit", "params.n", "params.kind"]
    );
    assert_eq!(
        r.substitute(&map(json!({"unit":"u","kind":"a"})))
            .unwrap_err()[0]
            .path,
        "params.n"
    );
    assert_eq!(
        r.substitute(&map(json!({"n":1,"kind":"a"}))).unwrap_err()[0].path,
        "params.unit"
    );
}
#[test]
fn parameter_declarations_reuse_model_types_and_docs() {
    let r = recipe(
        json!({"a":{}}),
        json!({"n":{"type":"int","doc":"A count"},"rec":{"type":"record","fields":{"name":"string"}}}),
    );
    assert_eq!(r.params()["n"].doc.as_deref(), Some("A count"));
    assert!(
        r.substitute(&map(json!({"unit":"u","n":2,"rec":{"name":"ok"}})))
            .is_ok()
    );
    for form in [
        json!({"unit":"int"}),
        json!({"Bad":"int"}),
        json!({"n":"strng"}),
        json!({"n":{"doc":"missing type"}}),
    ] {
        assert!(Recipe::parse("r", &json!({"name":"r","params":form,"steps":{"a":{}}})).is_err());
    }
}
#[test]
fn strict_json_and_broken_catalog_entries_do_not_hide_others() {
    let global = br#"{"name":"lane","doc":"global","steps":{"{unit}":{"run":"core.external"}}}"#;
    let project =
        br#"{"name":"lane","doc":"project","steps":{"{unit}-mine":{"run":"core.external"}}}"#;
    let good = br#"{"name":"good","steps":{"{unit}":{"run":"core.external"}}}"#;
    let old = br#"{"name":"old","steps":{"{unit}":{"run":"core.external","when":"yes"}}}"#;
    let entries = catalog([
        ("lane", "global", global.as_slice()),
        ("good", "global", good.as_slice()),
        ("lane", "project", project.as_slice()),
        ("broken", "project", b"{".as_slice()),
        ("old", "project", old.as_slice()),
    ]);
    assert_eq!(entries["lane"].scope, "project");
    assert_eq!(entries["lane"].recipe.as_ref().unwrap().doc(), "project");
    assert!(entries["good"].recipe.is_ok());
    assert!(entries["broken"].recipe.is_err());
    assert!(
        entries["old"].recipe.as_ref().unwrap_err()[0]
            .path
            .ends_with(".when")
    );
    for bytes in [
        br#"{"name":"r","name":"r","steps":{}}"#.as_slice(),
        b"[]",
        br#"{"name":"r","steps":{"a":{"n":9223372036854775808}}}"#,
    ] {
        assert!(Recipe::parse_json("r", bytes).is_err());
    }
    let shadowed = catalog([
        ("lane", "global", global.as_slice()),
        ("lane", "project", b"{".as_slice()),
    ]);
    assert!(shadowed["lane"].recipe.is_err());
}
#[test]
fn substitution_refuses_key_and_step_id_collisions() {
    let r = recipe(json!({"{unit}":{}, "a":{}}), json!({}));
    assert!(
        r.substitute(&map(json!({"unit":"a"})))
            .unwrap_err()
            .iter()
            .any(|e| e.message.contains("duplicate key"))
    );
    let r = recipe(
        json!({"a":{"in":{"{key}":1,"value":2}}}),
        json!({"key":"string"}),
    );
    assert!(
        r.substitute(&map(json!({"unit":"u","key":"value"})))
            .is_err()
    );
}

#[test]
fn escaped_keys_are_checked_for_collisions_only_after_real_substitution() {
    let r = recipe(
        json!({"a":{"in":{"{key}":1,"{{key}}":2}}}),
        json!({"key":"string"}),
    );
    assert_eq!(
        value(&r.substitute(&map(json!({"unit":"u","key":"x"}))).unwrap())["a"]["in"],
        json!({"x":1,"{key}":2})
    );
    assert!(
        r.substitute(&map(json!({"unit":"u","key":"{key}"})))
            .is_err()
    );
}

#[test]
fn value_parser_refuses_overflow_and_bounds_placeholder_recursion() {
    assert!(Recipe::parse("r", &json!({"name":"r","steps":{"a":{"n":u64::MAX}}})).is_err());
    let mut nested = json!("{unit}");
    for _ in 0..130 {
        nested = json!([nested]);
    }
    let errors = Recipe::parse("r", &json!({"name":"r","steps":{"a":{"doc":nested}}})).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("nesting exceeds 128"))
    );
}
