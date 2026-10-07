//! Strict core.format template contract.
use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::rpc::{JsonMap, decode_json};
use sluice_runtime::builtins::{
    core,
    jev::{BuiltinCtx, FnFailure},
};

fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn ctx() -> BuiltinCtx {
    BuiltinCtx {
        env: IndexMap::new(),
    }
}

async fn dispatch(name: &str, inputs: Value) -> Result<JsonMap, FnFailure> {
    core::dispatch(name, &map(inputs), &ctx()).await
}
fn out(map: &JsonMap, name: &str) -> Value {
    map.0.get(name).unwrap().as_value().clone()
}
fn failed(result: Result<JsonMap, FnFailure>) -> String {
    match result {
        Err(FnFailure::Terminal(message)) => message,
        Err(FnFailure::Transient(message)) => panic!("unexpected transient failure: {message}"),
        Err(FnFailure::NotBuilt(name)) => panic!("unexpected not-built failure: {name}"),
        Ok(value) => panic!("expected a terminal failure, got {value:?}"),
    }
}

#[tokio::test]
async fn format_fills_record_names_and_renders_non_strings_as_compact_json() {
    let result = dispatch(
        "core.format",
        json!({"template": "hi {who}, {n} + {rest} = {sum}",
               "values": {"who": "world", "n": 4, "rest": [1, 2], "sum": {"a": true}}}),
    )
    .await
    .unwrap();
    assert_eq!(out(&result, "text"), "hi world, 4 + [1,2] = {\"a\":true}");
}

#[tokio::test]
async fn format_fills_positions_from_arrays_or_a_single_value() {
    let result = dispatch(
        "core.format",
        json!({"template": "{0} then {1}", "values": ["first", 2]}),
    )
    .await
    .unwrap();
    assert_eq!(out(&result, "text"), "first then 2");
    let single = dispatch("core.format", json!({"template": "{0}!", "values": 7}))
        .await
        .unwrap();
    assert_eq!(out(&single, "text"), "7!");
    assert!(
        failed(dispatch("core.format", json!({"template": "{1}", "values": "one"})).await)
            .contains("single value")
    );
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "{2}", "values": ["a", "b"]})
            )
            .await
        )
        .contains("has 2 item(s)")
    );
}

#[tokio::test]
async fn format_escapes_braces_and_rejects_lone_ones() {
    let result = dispatch(
        "core.format",
        json!({"template": "{{{0}}} {{literal}} }}", "values": ["x"]}),
    )
    .await
    .unwrap();
    assert_eq!(out(&result, "text"), "{x} {literal} }");
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "unclosed {", "values": {}})
            )
            .await
        )
        .contains("unclosed '{'")
    );
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "{a{b}", "values": {"a": 1, "b": 2}})
            )
            .await
        )
        .contains("unclosed '{'")
    );
    assert!(
        failed(dispatch("core.format", json!({"template": "stray }", "values": {}})).await)
            .contains("unmatched '}'")
    );
    assert!(
        failed(dispatch("core.format", json!({"template": "empty {}", "values": {}})).await)
            .contains("empty placeholder")
    );
}

#[tokio::test]
async fn format_refuses_python_format_syntax_and_wrong_value_shapes() {
    for (template, refused) in [
        ("{x.y}", "attribute or item access"),
        ("{x[0]}", "attribute or item access"),
        ("{x!r}", "conversion flags"),
        ("{x!s:>10}", "conversion flags"),
        ("{x:>10}", "format specifications"),
        ("{x:.2f}", "format specifications"),
    ] {
        let message = failed(
            dispatch(
                "core.format",
                json!({"template": template, "values": {"x": 1}}),
            )
            .await,
        );
        assert!(message.contains(refused), "{template}: {message}");
    }
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "{missing}", "values": {"x": 1}})
            )
            .await
        )
        .contains("no value 'missing'")
    );
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "{0}", "values": {"x": 1}})
            )
            .await
        )
        .contains("is a record")
    );
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "{name}", "values": ["a"]})
            )
            .await
        )
        .contains("is an array")
    );
}
