//! The agent fns' `model` object: composition for every shape, the engine defaults, the check
//! against each engine's listing (with the nearest ids), and the retired string and `effort`
//! forms refused with the object to use.
use serde_json::{Value, json};
use sluice_agents::{
    AgentBuiltinRequest,
    engines::devin,
    model::{self, ModelChoice, ResolvedModel},
    prompt::PromptContext,
    supervisor::FailureKind,
};
use sluice_model::rpc::{JsonMap, JsonValue};

fn choice(value: Value) -> ModelChoice {
    ModelChoice::parse(&value).unwrap()
}
fn id(engine: &str, value: Value) -> String {
    model::compose(engine, &choice(value)).unwrap().id
}
fn devin_models() -> Vec<String> {
    devin::profile::parse_models(devin::fixture::MODELS.as_bytes()).unwrap()
}

#[test]
fn every_shape_composes_the_id_its_engine_takes() {
    for (value, expected) in [
        (
            json!({"type":"normal","model":"swe-2","effort":"high"}),
            "swe-2-high",
        ),
        (json!({"type":"normal","model":"adaptive"}), "adaptive"),
        (
            json!({"type":"normal","model":"claude-opus-5-5","effort":"high","fast":true}),
            "claude-opus-5-5-high-fast",
        ),
        (
            json!({"type":"normal","model":"swe-2","effort":"high","fast":false}),
            "swe-2-high",
        ),
        (
            json!({"type":"fusion","main":{"model":"claude-opus-5-5","effort":"high","fast":false},"sidekick":{"model":"swe-2","effort":"high"}}),
            "fusion-claude-opus-5-5-high-sidekick-swe-2-high",
        ),
        (
            json!({"type":"fusion","main":{"model":"claude-opus-5-5","effort":"high","fast":true},"sidekick":{"model":"swe-2","effort":"high"}}),
            "fusion-claude-opus-5-5-high-fast-sidekick-swe-2-high",
        ),
        (
            json!({"type":"fusion","main":{"model":"claude-opus-5-5","effort":"high"},"sidekick":{"model":"glm-5-2"}}),
            "fusion-claude-opus-5-5-high-sidekick-glm-5-2",
        ),
        (
            json!({"type":"fusion","main":{"model":"claude-opus-5-5","effort":"high","fast":true},"sidekick":{"model":"gpt-5-6-luna","effort":"high","priority":true}}),
            "fusion-claude-opus-5-5-high-fast-sidekick-gpt-5-6-luna-high-priority",
        ),
    ] {
        assert_eq!(id("devin", value.clone()), expected, "{value}");
        // Every composed id is one the real listing (captured as the fixture) offers.
        assert!(devin_models().contains(&expected.to_owned()), "{expected}");
    }
    assert_eq!(
        model::compose(
            "codex",
            &choice(json!({"type":"normal","model":"sol","effort":"high"}))
        )
        .unwrap(),
        ResolvedModel {
            id: "gpt-6.1-sol@high".into(),
            model: "gpt-6.1-sol".into(),
            effort: Some("high".into()),
        }
    );
    assert_eq!(
        id(
            "codex",
            json!({"type":"normal","model":"astra","effort":"max"})
        ),
        "gpt-6-astra@max"
    );
    assert_eq!(
        id(
            "codex",
            json!({"type":"normal","model":"gpt-6-luna","effort":"xhigh"})
        ),
        "gpt-6-luna@xhigh"
    );
    assert_eq!(
        id("codex", json!({"type":"normal","model":"sol"})),
        "gpt-6.1-sol"
    );
    assert_eq!(
        id(
            "claude",
            json!({"type":"normal","model":"claude-opus-5-5","effort":"max"})
        ),
        "claude-opus-5-5@max"
    );
    for (engine, value, message) in [
        (
            "codex",
            json!({"type":"normal","model":"sol","effort":"high","fast":true}),
            "codex cannot run a model fast",
        ),
        (
            "claude",
            json!({"type":"normal","model":"opus","effort":"high","fast":true}),
            "claude cannot run a model fast",
        ),
        (
            "codex",
            json!({"type":"fusion","main":{"model":"m","effort":"high"},"sidekick":{"model":"s"}}),
            "fusion runs only on devin",
        ),
        (
            "claude",
            json!({"type":"fusion","main":{"model":"m","effort":"high"},"sidekick":{"model":"s"}}),
            "fusion runs only on devin",
        ),
    ] {
        let error = model::compose(engine, &choice(value)).unwrap_err();
        assert!(error.contains(message), "{error}");
    }
}

fn request(name: &str, inputs: Value) -> Result<AgentBuiltinRequest, String> {
    let inputs = JsonMap(
        inputs
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), JsonValue::try_from(v.clone()).unwrap()))
            .collect(),
    );
    AgentBuiltinRequest::build(name, &inputs, &PromptContext::default()).map_err(|e| {
        assert_eq!(e.kind, FailureKind::Invalid, "{e}");
        e.message
    })
}

#[test]
fn retired_string_models_and_effort_are_refused_with_the_object_to_use() {
    for (name, inputs, hint) in [
        (
            "agent.run",
            json!({"engine":"codex","cwd":"/tmp","spec":"x","model":"sol","effort":"xhigh"}),
            r#"model must be a JSON object, not "sol", and effort is no longer an input: drop effort and set model to {"type":"normal","model":"sol","effort":"xhigh"}"#,
        ),
        (
            "agent.codex",
            json!({"cwd":"/tmp","spec":"x","model":"astra"}),
            r#"model must be a JSON object, not "astra"; use {"type":"normal","model":"astra","effort":"high"}"#,
        ),
        (
            "agent.run",
            json!({"engine":"devin","cwd":"/tmp","spec":"x","model":"fusion"}),
            r#"use {"type":"fusion","main":{"model":"claude-opus-5-5","effort":"high"},"sidekick":{"model":"swe-2","effort":"high"}}"#,
        ),
        (
            "agent.devin",
            json!({"cwd":"/tmp","spec":"x","model":"fusion-claude-opus-5-5-high-fast-sidekick-gpt-5-6-luna-high-priority"}),
            r#"use {"type":"fusion","main":{"model":"claude-opus-5-5","effort":"high","fast":true},"sidekick":{"model":"gpt-5-6-luna","effort":"high","priority":true}}"#,
        ),
        (
            "agent.devin",
            json!({"cwd":"/tmp","spec":"x","model":"swe-2-high"}),
            r#"use {"type":"normal","model":"swe-2","effort":"high"}"#,
        ),
        (
            "agent.devin",
            json!({"cwd":"/tmp","spec":"x","model":"high"}),
            r#"use {"type":"normal","model":"swe-2","effort":"high"}"#,
        ),
        (
            "agent.devin",
            json!({"cwd":"/tmp","spec":"x","effort":"max"}),
            r#"effort is no longer an input: drop it and set model to {"type":"normal","model":"swe-2","effort":"max"}"#,
        ),
        (
            "agent.codex",
            json!({"cwd":"/tmp","spec":"x","model":{"type":"normal","model":"sol","effort":"high"},"effort":"low"}),
            r#"effort is no longer an input: drop it and set model to {"type":"normal","model":"sol","effort":"low"}"#,
        ),
        (
            "agent.claude",
            json!({"cwd":"/tmp","prompt":"x","model":"opus"}),
            r#"use {"type":"normal","model":"opus","effort":"high"}"#,
        ),
        (
            "agent.run",
            json!({"engine":"claude","cwd":"/tmp","spec":"x","model":7}),
            r#"model must be a JSON object, not 7; use {"type":"normal","model":"opus","effort":"high"}"#,
        ),
        (
            "agent.run",
            json!({"engine":"codex","cwd":"/tmp","spec":"x","model":{"type":"fusion","main":{"model":"m","effort":"high"},"sidekick":{"model":"s"}}}),
            "fusion runs only on devin",
        ),
        (
            "agent.devin",
            json!({"cwd":"/tmp","spec":"x","model":{"type":"normal","model":"swe-2","speed":"fast"}}),
            r#"model has no field "speed""#,
        ),
    ] {
        let error = request(name, inputs.clone()).unwrap_err();
        assert!(error.contains(hint), "{name} {inputs}: {error}");
    }
    let ok = request(
        "agent.run",
        json!({"engine":"devin","cwd":"/tmp","spec":"x","model":{"type":"fusion","main":{"model":"claude-opus-5-5","effort":"high","fast":true},"sidekick":{"model":"swe-2","effort":"high"}},"effort":null}),
    )
    .unwrap();
    assert!(matches!(ok.model, Some(ModelChoice::Fusion { .. })));
    assert_eq!(
        request("agent.codex", json!({"cwd":"/tmp","spec":"x","model":null}))
            .unwrap()
            .model,
        None
    );
}
