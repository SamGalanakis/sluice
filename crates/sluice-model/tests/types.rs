use indexmap::IndexMap;
use proptest::prelude::*;
use serde_json::{Value, json};
use sluice_model::types::{
    Type, check_value, check_value_at, decode_json, fits, navigate, navigate_segments,
    navigate_value,
};

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
fn structural_fits_accepts_python_scenarios() {
    for (source, target) in [
        (json!("int"), json!("int")),
        (json!("int"), json!("float")),
        (json!("string"), json!("Any")),
        (json!("Any"), json!("int")),
        (json!("int"), json!("int?")),
        (json!("int?"), json!("int?")),
        (json!("int[]"), json!("float[]")),
        (
            json!({"type":"enum","symbols":["a"]}),
            json!({"type":"enum","symbols":["a","b"]}),
        ),
        (json!({"type":"enum","symbols":["a"]}), json!("string")),
        (
            json!({"type":"record","fields":{"branch":"string","sha":"string","x":"int"}}),
            record(false),
        ),
        (
            json!({"type":"record","fields":{"branch":"string"}}),
            record(false),
        ),
    ] {
        assert!(
            fits(&p(source.clone()), &p(target.clone())),
            "{source} -> {target}"
        );
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
fn values_accept_python_scenarios() {
    for (form, value) in [
        (json!("int"), json!(3)),
        (json!("float"), json!(3)),
        (json!("float"), json!(2.5)),
        (json!("boolean"), json!(false)),
        (json!("Any"), json!({"x":[1]})),
        (json!("Any?"), Value::Null),
        (json!("string?"), Value::Null),
        (json!({"type":"enum","symbols":["done"]}), json!("done")),
        (json!("int[]"), json!([1, 2])),
        (record(false), json!({"branch":"b"})),
        (record(false), json!({"branch":"b","extra":1})),
        (json!("Any"), Value::Null),
    ] {
        assert_eq!(check_value(&p(form), &value), Ok(()));
    }
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
fn check_value_prefixes_a_base_path() {
    let errors = check_value_at(&Type::Int, &json!("x"), "steps.a.in.n").unwrap_err();
    assert_eq!(
        errors[0].to_string(),
        "steps.a.in.n: expected int, got \"x\""
    );
}

#[test]
fn navigation_types_and_values() {
    let t = p(
        json!({"type":"record","fields":{"head":record(false),"meta":"Any","tags":"string[]",
        "opt":["null",{"type":"record","fields":{"x":"int"}}]}}),
    );
    for (path, expected) in [
        ("head.branch", p(json!("string"))),
        ("head.sha", p(json!("string?"))),
        ("meta.deep.0", Type::Any),
        ("tags.0", Type::String),
        ("opt.x", p(json!("int?"))),
    ] {
        assert_eq!(navigate(&t, path).unwrap(), expected);
    }
    assert_eq!(navigate(&t, "head.nope").unwrap_err().path, "head.nope");
    assert!(navigate(&t, "tags.x").is_err());
    assert_eq!(navigate(&t, "").unwrap(), t);
    let v = json!({"a":{"b":[10,20]},"n":null});
    assert_eq!(navigate_value(&v, &["a", "b", "1"]), Some(&json!(20)));
    assert_eq!(navigate_value(&v, &["a", "b", "5"]), None);
    assert_eq!(navigate_value(&v, &["n", "x"]), None);
    assert_eq!(navigate_value(&v, &["n"]), Some(&Value::Null));
    assert_eq!(navigate_value(&v, &[]), Some(&v));
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
fn form_spells_types_back_and_display_is_parseable() {
    for form in [
        json!("string"),
        json!("Any"),
        json!("int?"),
        json!("string[]"),
        json!("float[]?"),
        json!(["null",{"type":"enum","symbols":["a","b"]}]),
        json!({"type":"array","items":{"type":"record","fields":{"a":"int","b":"string?"}}}),
        json!({"type":"array","items":["null",{"type":"enum","symbols":["x"]}]}),
    ] {
        let t = p(form.clone());
        assert_eq!(t.form(), form);
        assert_eq!(t.to_string().parse::<Type>().unwrap(), t);
        assert_eq!(Type::parse_json(t.to_string().as_bytes()).unwrap(), t);
    }
    assert_eq!(
        p(json!({"type":"array","items":"int"})).form(),
        json!("int[]")
    );
    assert_eq!("int[]".parse::<Type>().unwrap(), p(json!("int[]")));
    let escaped = Type::Record(IndexMap::from([(
        "a\"\\\n".into(),
        Type::Enum(vec!["é\"".into()]),
    )]));
    assert_eq!(escaped.to_string().parse::<Type>().unwrap(), escaped);
}

#[test]
fn strict_json_boundaries_reject_duplicates_overflow_and_nonfinite() {
    for bytes in [
        r#"{"x":1,"x":2}"#,
        r#"{"x":[{"n":1,"n":2}]}"#,
        r#"{"\u0061":1,"a":2}"#,
        "9223372036854775808",
        "-9223372036854775809",
        "18446744073709551616",
        "-18446744073709551616",
        "NaN",
        "Infinity",
        "-Infinity",
        "1e999",
        "null null",
    ] {
        assert!(decode_json::<Value>(bytes.as_bytes()).is_err(), "{bytes}");
    }
    for bytes in [
        "9223372036854775807",
        "-9223372036854775808",
        "1.0",
        "1e20",
        "1.7976931348623157e308",
    ] {
        let value: Value = decode_json(bytes.as_bytes()).unwrap();
        assert!(check_value(&Type::Any, &value).is_ok());
    }
    for bytes in [
        r#"{"type":"record","fields":{"a":"int","a":"string"}}"#,
        r#"{"type":"enum","symbols":[9223372036854775808]}"#,
        r#"{"type":"record","fields":{"a":1e999}}"#,
    ] {
        assert!(Type::parse_json(bytes.as_bytes()).is_err());
    }
    for value in [json!(u64::MAX), json!({"extra":[u64::MAX]})] {
        assert!(Type::parse(&value).is_err());
        assert!(check_value(&Type::Any, &value).is_err());
        assert!(check_value(&p(json!({"type":"record","fields":{}})), &value).is_err());
    }
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

fn type_strategy() -> impl Strategy<Value = Type> {
    prop_oneof![
        Just(Type::String),
        Just(Type::Int),
        Just(Type::Float),
        Just(Type::Boolean),
        Just(Type::Any),
        proptest::collection::btree_set("[a-z]{1,5}", 1..5)
            .prop_map(|s| Type::Enum(s.into_iter().collect()))
    ]
    .prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            inner.clone().prop_map(|t| Type::Optional(Box::new(t))),
            inner.clone().prop_map(|t| Type::List(Box::new(t))),
            proptest::collection::btree_map("[a-z]{1,5}", inner, 0..5)
                .prop_map(|m| Type::Record(m.into_iter().collect())),
        ]
    })
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    #[test]
    fn parse_display_roundtrip(t in type_strategy()) {
        let wire = serde_json::to_vec(&t).unwrap();
        prop_assert_eq!(Type::parse_json(&wire).unwrap(), t.clone());
        prop_assert_eq!(decode_json::<Type>(&wire).unwrap(), t.clone());
        prop_assert_eq!(t.to_string().parse::<Type>().unwrap(), t);
    }
    #[test]
    fn fits_is_reflexive(t in type_strategy()) { prop_assert!(fits(&t,&t)); }
}
