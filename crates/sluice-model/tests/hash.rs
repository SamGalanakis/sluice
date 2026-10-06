use indexmap::IndexMap;
use proptest::prelude::*;
use serde_json::{Value, json};
use sluice_model::{
    hash::{EffectiveInput, ExecutionProvenance, InputsHash, canonical_json},
    rpc::{JsonMap, JsonValue, decode_json},
};

fn hash(value: Value) -> InputsHash {
    InputsHash::parse_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn data(value: Value) -> EffectiveInput {
    EffectiveInput::Value(JsonValue::try_from(value).unwrap())
}

#[test]
fn canonical_form_has_exact_tags_numbers_order_and_escaping() {
    let value: Value = decode_json(br#"{"z":[null,true,1,1.0],"a":{"z":"x\n","a":-2}}"#).unwrap();
    assert_eq!(
        String::from_utf8(canonical_json(&value).unwrap()).unwrap(),
        r#"["object",{"a":["object",{"a":["int","-2"],"z":["string","x\n"]}],"z":["array",[["null"],["boolean",true],["int","1"],["float","3ff0000000000000"]]]}]"#
    );
    assert_eq!(
        canonical_json(&json!("é/\"\\\t")).unwrap(),
        r#"["string","é/\"\\\t"]"#.as_bytes()
    );
}

#[test]
fn versioned_hash_matches_independent_sha256_fixture() {
    // Computed independently using Python hashlib over the documented bytes.
    assert_eq!(
        hash(json!({})).to_string(),
        "b7efcdc153ac1f8525258ba2e6a75b268f9cf0ef81f4be6fff04231140cf26d5"
    );
    assert_eq!(
        hash(json!({"x":1})).to_string(),
        "fe00266ab90c4312dfe074a3a90df3a884570ed6b5e8e85e0393a7afd29be85b"
    );
}

#[test]
fn file_edits_affect_provenance_but_keep_validity_hash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("brief.md");
    std::fs::write(&path, b"one").unwrap();
    let bindings = IndexMap::from([(
        "brief".into(),
        EffectiveInput::File(path.to_str().unwrap().into()),
    )]);
    let before = InputsHash::from_bindings(&bindings).unwrap();
    let old_bytes = std::fs::read(&path).unwrap();
    let provenance = ExecutionProvenance::fingerprint(&old_bytes);
    std::fs::write(&path, b"two").unwrap();
    assert_eq!(InputsHash::from_bindings(&bindings).unwrap(), before);
    assert_ne!(
        ExecutionProvenance::fingerprint(&std::fs::read(&path).unwrap()),
        provenance
    );
    assert_eq!(ExecutionProvenance::fingerprint(&old_bytes), provenance);
    assert_eq!(
        before,
        hash(json!({"brief":{"file":path.to_str().unwrap()}}))
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(InputsHash::from_bindings(&bindings).unwrap(), before);
}

#[test]
fn changed_file_path_changes_hash_even_with_identical_bytes() {
    let a = IndexMap::from([("brief".into(), EffectiveInput::File("/tmp/a.md".into()))]);
    let b = IndexMap::from([("brief".into(), EffectiveInput::File("/tmp/b.md".into()))]);
    assert_ne!(
        InputsHash::from_bindings(&a).unwrap(),
        InputsHash::from_bindings(&b).unwrap()
    );
}

#[test]
fn unbound_optional_inputs_are_absent_and_bound_null_is_present() {
    let declarations_before = ["path"];
    let declarations_after = ["path", "base"];
    let bindings = IndexMap::from([("path".into(), data(json!("/tmp/source.txt")))]);
    let resolve_bound = |declarations: &[&str]| {
        declarations
            .iter()
            .filter_map(|name| bindings.get(*name).map(|v| ((*name).to_owned(), v.clone())))
            .collect::<IndexMap<_, _>>()
    };
    let before = InputsHash::from_bindings(&resolve_bound(&declarations_before)).unwrap();
    assert_eq!(
        before,
        InputsHash::from_bindings(&resolve_bound(&declarations_after)).unwrap()
    );
    let mut with_null = bindings.clone();
    with_null.insert("base".into(), data(Value::Null));
    assert_ne!(before, InputsHash::from_bindings(&with_null).unwrap());
    with_null.insert("base".into(), data(json!("x")));
    assert_ne!(before, InputsHash::from_bindings(&with_null).unwrap());
}

#[test]
fn effective_literals_defaults_plan_inputs_and_handoffs_hash_by_value() {
    let effective = IndexMap::from([("a".into(), data(json!(2))), ("b".into(), data(json!(1)))]);
    let original = InputsHash::from_bindings(&effective).unwrap();
    assert_eq!(original, hash(json!({"b":1,"a":2})));
    assert_ne!(original, hash(json!({"a":2,"b":5})));
    assert_ne!(original, hash(json!({"a":5,"b":1})));
    assert_eq!(original, hash(json!({"a":2,"b":1})));
}

#[test]
fn gates_and_fn_edits_do_not_contribute_to_effective_data() {
    let first = json!({"after":["a"],"run":"old.fn","effective":{"n":2}});
    let second = json!({"after":["!check/ok","unit:other?"],"run":"new.fn","effective":{"n":2}});
    assert_eq!(
        hash(first["effective"].clone()),
        hash(second["effective"].clone())
    );
    assert_ne!(hash(first["effective"].clone()), hash(json!({"n":3})));
}

#[test]
fn object_order_is_irrelevant_at_all_depths_and_array_order_matters() {
    let a = InputsHash::parse_json(br#"{"b":2,"a":[{"z":0,"x":1}]}"#).unwrap();
    let b = InputsHash::parse_json(br#"{"a":[{"x":1,"z":0}],"b":2}"#).unwrap();
    assert_eq!(a, b);
    assert_ne!(hash(json!({"items":[1,2]})), hash(json!({"items":[2,1]})));
    assert_ne!(hash(json!({"items":[1,2]})), hash(json!({"items":[1,2,3]})));
}

#[test]
fn numeric_representation_is_explicit_and_collision_free() {
    assert_ne!(hash(json!({"x":1})), hash(json!({"x":1.0})));
    assert_ne!(hash(json!({"x":0.0})), hash(json!({"x":-0.0})));
    assert_eq!(
        InputsHash::parse_json(br#"{"x":1.0}"#).unwrap(),
        InputsHash::parse_json(br#"{"x":1e0}"#).unwrap()
    );
    assert_ne!(hash(json!({"x":1})), hash(json!({"x":["int","1"]})));
    assert_ne!(
        hash(json!({"x":1.0})),
        hash(json!({"x":"3ff0000000000000"}))
    );
    assert_ne!(hash(json!({"x":true})), hash(json!({"x":1})));
    assert_ne!(hash(json!({"x":i64::MAX})), hash(json!({"x":i64::MAX-1})));
    assert_ne!(hash(json!({"x":i64::MIN})), hash(json!({"x":i64::MIN+1})));
}

#[test]
fn strict_hash_ingress_refuses_invalid_json_and_nesting() {
    for input in [
        r#"{"x":1,"x":2}"#,
        r#"{"x":[{"a":1,"a":2}]}"#,
        r#"{"x":9223372036854775808}"#,
        r#"{"x":18446744073709551616}"#,
        r#"{"x":-9223372036854775809}"#,
        r#"{"x":1e999}"#,
        r#"{"x":NaN}"#,
        r#"{"x":Infinity}"#,
        r#"{} {}"#,
        "null",
        "[]",
        "1",
    ] {
        assert!(InputsHash::parse_json(input.as_bytes()).is_err(), "{input}");
    }
    assert!(canonical_json(&json!({"x":[u64::MAX]})).is_err());
    let mut nested = Value::Null;
    for _ in 0..128 {
        nested = json!([nested]);
    }
    assert!(canonical_json(&nested).is_err());
}

#[test]
fn digest_serialization_roundtrips_and_refuses_malformed_digests() {
    let digest = hash(json!({"n":1}));
    assert_eq!(digest.as_bytes().len(), 32);
    assert_eq!(digest.to_string().parse::<InputsHash>().unwrap(), digest);
    assert_eq!(
        decode_json::<InputsHash>(&serde_json::to_vec(&digest).unwrap()).unwrap(),
        digest
    );
    let provenance = ExecutionProvenance::fingerprint(b"abc");
    assert_eq!(
        provenance.to_string(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        decode_json::<ExecutionProvenance>(&serde_json::to_vec(&provenance).unwrap()).unwrap(),
        provenance
    );
    assert_eq!(provenance.as_bytes().len(), 32);
    for malformed in [
        "".to_owned(),
        "a".repeat(63),
        "0".repeat(65),
        "A".repeat(64),
        "é".repeat(32),
        "g".repeat(64),
    ] {
        assert!(malformed.parse::<InputsHash>().is_err());
        assert!(
            decode_json::<ExecutionProvenance>(&serde_json::to_vec(&malformed).unwrap()).is_err()
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    #[test]
    fn hashes_are_stable_under_nested_key_reordering(entries in proptest::collection::btree_map("[a-z]{1,6}",any::<i64>(),0..16)) {
        let nested: serde_json::Map<_,_> = entries.iter().map(|(k,v)|(k.clone(),json!({"z":v,"a":[v,true]}))).collect();
        let reversed: serde_json::Map<_,_> = entries.iter().rev().map(|(k,v)|(k.clone(),json!({"a":[v,true],"z":v}))).collect();
        let first: JsonMap = decode_json(&serde_json::to_vec(&nested).unwrap()).unwrap();
        let second: JsonMap = decode_json(&serde_json::to_vec(&reversed).unwrap()).unwrap();
        prop_assert_eq!(InputsHash::of(&first).unwrap(),InputsHash::of(&second).unwrap());
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(1024))]
    #[test]
    fn finite_float_bits_and_hash_survive_strict_json_roundtrips(bits in proptest::prelude::any::<u64>()) {
        let value = f64::from_bits(bits);
        proptest::prop_assume!(value.is_finite());
        let first: JsonMap = decode_json(&serde_json::to_vec(&json!({"x": value})).unwrap()).unwrap();
        proptest::prop_assert_eq!(first.0["x"].as_value().as_f64().unwrap().to_bits(), bits);
        let encoded = serde_json::to_vec(&first).unwrap();
        let second: JsonMap = decode_json(&encoded).unwrap();
        proptest::prop_assert_eq!(second.0["x"].as_value().as_f64().unwrap().to_bits(), bits);
        proptest::prop_assert_eq!(InputsHash::of(&first).unwrap(), InputsHash::of(&second).unwrap());
    }
}

#[test]
fn structural_data_comparison_matches_canonical_numeric_and_order_rules() {
    use sluice_model::hash::data_equal;
    for (left, right, equal) in [
        (json!({"x":[-0.0]}), json!({"x":[0.0]}), false),
        (json!({"x":[1]}), json!({"x":[1.0]}), false),
        (
            json!({"x":1,"y":{"b":2,"a":3}}),
            json!({"y":{"a":3,"b":2},"x":1}),
            true,
        ),
        (json!([1, 2]), json!([2, 1]), false),
    ] {
        assert_eq!(data_equal(&left, &right).unwrap(), equal);
    }
}

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-3_i64..3).prop_map(|n| json!(n)),
        prop_oneof![Just(0.0), Just(-0.0), Just(1.0), Just(1.5), Just(-2.25)]
            .prop_map(|f| json!(f)),
        "[ab\"\\\\\n]{0,3}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 24, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            proptest::collection::vec(("[abc]{1,2}", inner), 0..4)
                .prop_map(|entries| Value::Object(entries.into_iter().collect())),
        ]
    })
}
/// The same value with every object's keys in reverse order.
fn reordered(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(reordered).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .rev()
                .map(|(k, v)| (k.clone(), reordered(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    /// `data_equal` compares in place; it must agree with comparing canonical encodings.
    #[test]
    fn structural_comparison_agrees_with_canonical_encodings(left in json_value(), other in json_value(), same in any::<bool>()) {
        let right = if same { reordered(&left) } else { other };
        let canonical = canonical_json(&left).unwrap() == canonical_json(&right).unwrap();
        prop_assert_eq!(sluice_model::hash::data_equal(&left, &right).unwrap(), canonical);
    }
}

/// The inputs hash as specified: SHA-256 of the format envelope around `canonical_json`.
fn specified(inputs: &serde_json::Map<String, Value>) -> String {
    use sha2::{Digest, Sha256};
    let mut bytes = br#"{"format":"sluice-inputs-v1","inputs":"#.to_vec();
    bytes.extend(canonical_json(&Value::Object(inputs.clone())).unwrap());
    bytes.push(b'}');
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    /// The hash is streamed; it must be the specified bytes' hash.
    #[test]
    fn streamed_hashes_are_the_specified_hashes(
        entries in proptest::collection::vec(("[a-z\"\\\\]{1,3}", json_value(), proptest::option::of("[/a-z\"]{1,6}")), 0..5)
    ) {
        let mut values = serde_json::Map::new();
        let mut bindings = IndexMap::new();
        let mut as_objects = serde_json::Map::new();
        for (name, value, file) in entries {
            values.insert(name.clone(), value.clone());
            match file {
                Some(path) => {
                    as_objects.insert(name.clone(), json!({"file": path}));
                    bindings.insert(name, EffectiveInput::File(path));
                }
                None => {
                    as_objects.insert(name.clone(), value.clone());
                    bindings.insert(name, data(value));
                }
            }
        }
        let map: JsonMap = decode_json(&serde_json::to_vec(&values).unwrap()).unwrap();
        prop_assert_eq!(InputsHash::of(&map).unwrap().to_string(), specified(&values));
        prop_assert_eq!(InputsHash::from_bindings(&bindings).unwrap().to_string(), specified(&as_objects));
    }
}
