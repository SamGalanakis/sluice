use indexmap::IndexMap;
use proptest::prelude::*;
use serde_json::{Value, json};
use sluice_model::types::{Type, check_value, fits, navigate, navigate_segments, navigate_value};

fn p(form: Value) -> Type {
    Type::parse(&form).unwrap()
}
fn record(required_sha: bool) -> Value {
    json!({"type":"record","fields":{"branch":"string","sha":if required_sha {"string"} else {"string?"}}})
}

#[test]
fn parse_every_cwl_spelling() {
    use Type::*;
    let cases = [
        (json!("string"), String),
        (json!("Any"), Any),
        (json!("string?"), Optional(Box::new(String))),
        (json!(["null", "int"]), Optional(Box::new(Int))),
        (json!(["int", "null"]), Optional(Box::new(Int))),
        (json!("string[]"), List(Box::new(String))),
        (
            json!("string[]?"),
            Optional(Box::new(List(Box::new(String)))),
        ),
        (
            json!("string?[]"),
            List(Box::new(Optional(Box::new(String)))),
        ),
        (json!({"type":"array","items":"int"}), List(Box::new(Int))),
        (
            json!({"type":"enum","symbols":["a","b"]}),
            Enum(vec!["a".into(), "b".into()]),
        ),
        (
            json!(["null",{"type":"enum","symbols":["a"]}]),
            Optional(Box::new(Enum(vec!["a".into()]))),
        ),
        (
            json!({"type":"record","fields":{"a":"int","b":"string?"}}),
            Record(IndexMap::from([
                ("a".into(), Int),
                ("b".into(), Optional(Box::new(String))),
            ])),
        ),
        (json!("Any?"), Optional(Box::new(Any))),
    ];
    for (form, expected) in cases {
        assert_eq!(p(form), expected);
    }
}

#[test]
fn parse_errors_name_the_path() {
    for (form, path) in [
        (json!("str"), "type"),
        (json!("bool"), "type"),
        (json!({"type":"enum","symbols":[]}), "type.symbols"),
        (json!({"type":"record","fields":{"a":"nope"}}), "type.a"),
        (json!({"list":"int"}), "type"),
        (json!(["int", "string"]), "type"),
        (json!(3), "type"),
    ] {
        assert_eq!(Type::parse(&form).unwrap_err().path, path);
    }
    for form in [
        json!({"type":"enum","symbols":["a","a"]}),
        json!({"type":"enum","symbols":[1]}),
        json!({"type":"enum","symbols":"a"}),
        json!({"type":"array","items":"int","extra":false}),
        json!({"type":"record","fields":[]}),
        json!(["null"]),
        json!(["null", "int", "string"]),
        json!("null"),
        json!(""),
    ] {
        assert!(Type::parse(&form).is_err(), "{form}");
    }
}

#[test]
fn structural_fits_rejects_python_scenarios() {
    for (source, target) in [
        (json!("float"), json!("int")),
        (json!("string?"), json!("string")),
        (
            json!({"type":"enum","symbols":["a","c"]}),
            json!({"type":"enum","symbols":["a","b"]}),
        ),
        (json!("string"), json!({"type":"enum","symbols":["a"]})),
        (json!("string[]"), json!("int[]")),
        (record(false), record(true)),
        (
            json!({"type":"record","fields":{"branch":"string"}}),
            record(true),
        ),
        (json!("int"), json!("int[]")),
    ] {
        assert!(
            !fits(&p(source.clone()), &p(target.clone())),
            "{source} -> {target}"
        );
    }
    assert!(fits(&p(json!("string?")), &Type::Any));
    assert!(fits(&Type::Any, &p(json!("string?"))));
    assert!(!fits(&p(json!("int?[]")), &p(json!("float[]"))));
    assert!(fits(&p(json!("int[]")), &p(json!("float?[]"))));
}

#[test]
fn values_reject_with_paths_and_collect_all_fields() {
    for (form, value, paths, messages) in [
        (
            json!("int"),
            json!(true),
            vec![""],
            vec!["expected int, got true"],
        ),
        (
            json!("int"),
            json!(1.5),
            vec![""],
            vec!["expected int, got 1.5"],
        ),
        (
            json!("boolean"),
            json!(0),
            vec![""],
            vec!["expected boolean, got 0"],
        ),
        (
            json!("string"),
            Value::Null,
            vec![""],
            vec!["expected string, got null"],
        ),
        (
            json!({"type":"record","fields":{"report":{"type":"record","fields":{"outcome":{"type":"enum","symbols":["done","blocked"]}}}}}),
            json!({"report":{"outcome":"ok"}}),
            vec!["report.outcome"],
            vec!["expected one of [done, blocked], got \"ok\""],
        ),
        (
            record(true),
            json!({"sha":2}),
            vec!["branch", "sha"],
            vec!["missing required field", "expected string, got 2"],
        ),
        (
            json!("int[]"),
            json!([1, "2"]),
            vec!["[1]"],
            vec!["expected int, got \"2\""],
        ),
        (
            json!("int[]"),
            json!("12"),
            vec![""],
            vec!["expected an array, got \"12\""],
        ),
        (
            record(false),
            json!([1]),
            vec![""],
            vec!["expected an object, got [1]"],
        ),
    ] {
        let errors = check_value(&p(form), &value).unwrap_err();
        assert_eq!(
            errors.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
            paths
        );
        assert_eq!(
            errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>(),
            messages
        );
    }
    // Integral floats do not satisfy int even though they have no fractional part.
    assert!(check_value(&Type::Int, &json!(1.0)).is_err());
}

#[test]
fn invalid_list_indices_do_not_panic() {
    for index in ["²", "-1", "x", "", "99999999999999999999999999999999"] {
        if index.bytes().all(|b| b.is_ascii_digit()) && !index.is_empty() {
            assert_eq!(
                navigate(&p(json!("string[]")), index).unwrap(),
                Type::String
            );
        } else {
            assert!(navigate_segments(&p(json!("string[]")), &[index]).is_err());
        }
        assert_eq!(navigate_value(&json!(["a", "b"]), &[index]), None);
    }
}

#[test]
fn navigation_preserves_optional_ancestors_and_literal_field_names() {
    let t = p(json!(["null",{"type":"record","fields":{"a.b":"int?[]"}}]));
    assert_eq!(
        navigate_segments(&t, &["a.b", "0"]).unwrap(),
        p(json!("int?"))
    );
    assert_eq!(navigate(&p(json!("Any?")), "anything").unwrap(), Type::Any);
    assert!(navigate(&Type::Int, "field").is_err());
    assert!(navigate(&p(record(false)), "head..branch").is_err());
}

#[test]
fn type_and_value_nesting_is_bounded() {
    assert!(format!("int{}", "[]".repeat(128)).parse::<Type>().is_err());
    let mut value = Value::Null;
    for _ in 0..128 {
        value = json!([value]);
    }
    assert!(check_value(&Type::Any, &value).is_err());
    assert!(Type::parse(&value).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]


}
