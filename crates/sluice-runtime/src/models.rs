//! Edit-time refusal of an agent step whose `model` binds a string.
//!
//! An agent's `model` is a JSON object; a string (the retired form, e.g. `"sol"`) is refused
//! when the step launches, by `sluice_agents::model::from_inputs`. An edit that adds a step
//! with such a model, or changes a step's model to one, is refused the same way when it is
//! prepared, so it fails at the edit rather than at launch. Only new steps and changed models
//! are checked: a plan that already holds a string model still loads, validates and takes
//! every edit that leaves that model alone.
use serde_json::Value;
use sluice_model::{
    error::PublicError,
    gates::ValueRef,
    ids::StepId,
    plan::{Binding, Plan, Step},
    rpc::JsonMap,
    types::Type,
};

/// Whether a step's `model` input is an agent's model, checked when it launches: the step runs
/// an agent built-in, or an open fn (an agent block) that declares `model` as `Any`, the type
/// the agent built-ins give it, which it passes on to one.
fn agent_model(step: &Step) -> bool {
    if !step.signature.open {
        return false;
    }
    match step.signature.inputs.get("model") {
        Some(_) if sluice_agents::AGENT_BUILTINS.contains(&step.run.as_str()) => true,
        Some(Type::Any) => true,
        Some(Type::Optional(inner)) => matches!(**inner, Type::Any),
        _ => false,
    }
}

/// The engine the launch check names in its hint: fixed by the built-in, else the step's
/// literal `engine` (empty when it is not a literal engine name).
fn engine(step: &Step, inputs: &JsonMap) -> String {
    match step.run.as_str() {
        "agent.claude" | "agent.review" | "decide.llm" => return "claude".into(),
        "agent.codex" => return "codex".into(),
        "agent.devin" => return "devin".into(),
        _ => {}
    }
    resolve(step.bindings.get("engine"), inputs)
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|e| ["claude", "codex", "devin"].contains(&e.as_str()))
        .unwrap_or_default()
}

/// What a binding gives the step when it is known now: a literal, or a plan input's current
/// value. A step output, a file or a fan-in is known only when the step starts.
fn resolve(binding: Option<&Binding>, inputs: &JsonMap) -> Option<Value> {
    match binding? {
        Binding::Default(value) => Some(value.as_value().clone()),
        Binding::Source(reference) => plan_input(reference)
            .and_then(|name| inputs.0.get(&name))
            .map(|v| v.as_value().clone()),
        Binding::Sources(_) | Binding::File(_) => None,
    }
}

/// The plan input a reference reads whole, if it reads one.
fn plan_input(reference: &ValueRef) -> Option<String> {
    let parts = reference.parts().ok()?;
    (parts.step.is_none() && parts.fields.is_empty()).then_some(parts.name)
}

/// The launch's refusal of `step`'s model when it would be `model` (with the step's own
/// literal effort, as the launch sees it), or None when the launch would take it.
fn refusal(step: &Step, model: &Value, inputs: &JsonMap) -> Option<String> {
    if model.is_null() || model.is_object() {
        return None;
    }
    let effort = resolve(step.bindings.get("effort"), inputs);
    sluice_agents::model::from_inputs(&engine(step, inputs), Some(model), effort.as_ref()).err()
}

fn refuse(found: Vec<(String, String)>) -> Result<(), PublicError> {
    let Some((_, message)) = found.first() else {
        return Ok(());
    };
    Err(PublicError::Invalid {
        message: message.clone(),
        errors: found
            .iter()
            .map(|(path, message)| format!("{path}: {message}"))
            .collect(),
    })
}

/// Refuse an edit from `old` to `new` that adds an agent step whose model binds a string, or
/// changes an agent step's model (or its fn) so that it does. `written` are the steps the edit
/// writes (its `step.put`s): no other step is looked at, so the check costs what the edit
/// changes. `inputs` are the plan inputs' values the edit's preparation read. Steps whose fn
/// and model binding are unchanged are skipped.
pub(crate) fn check_edit<'a>(
    old: &Plan,
    new: &Plan,
    written: impl IntoIterator<Item = &'a StepId>,
    inputs: &JsonMap,
) -> Result<(), PublicError> {
    let mut found = vec![];
    for (id, step) in written
        .into_iter()
        .filter_map(|id| new.steps().get(id).map(|step| (id, step)))
    {
        if !agent_model(step) {
            continue;
        }
        let binding = step.bindings.get("model");
        if let Some(before) = old.steps().get(id)
            && before.run == step.run
            && before.bindings.get("model") == binding
        {
            continue;
        }
        if let Some(message) =
            resolve(binding, inputs).and_then(|model| refusal(step, &model, inputs))
        {
            found.push((format!("steps.{id}.in.model"), message));
        }
    }
    refuse(found)
}

/// Refuse setting plan input `name` to `value` when an agent step's model reads that input
/// whole and would be refused at launch with it.
pub(crate) fn check_input(
    plan: &Plan,
    name: &str,
    value: &Value,
    inputs: &JsonMap,
) -> Result<(), PublicError> {
    let mut found = vec![];
    for (id, step) in plan.steps() {
        if !agent_model(step) {
            continue;
        }
        let Some(Binding::Source(reference)) = step.bindings.get("model") else {
            continue;
        };
        if plan_input(reference).as_deref() != Some(name) {
            continue;
        }
        if let Some(message) = refusal(step, value, inputs) {
            found.push((format!("steps.{id}.in.model"), message));
        }
    }
    refuse(found)
}
