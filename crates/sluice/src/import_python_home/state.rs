use super::{
    convert, files,
    parser::{OldProject, Snapshot},
    sessions,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sluice_model::{
    Plan, StateSnapshot, StepState,
    commands::StepStatus,
    hash::EffectiveInput,
    rpc::{JsonMap, JsonValue},
    types::{SkipReason, check_value_at},
};
use std::path::Path;

pub fn collect_scatter(snapshot: &mut Snapshot, src: &Path) -> Result<()> {
    for project in &mut snapshot.projects {
        files::safe_component(&project.name)?;
        for (id, step) in project.plan["steps"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.iter())
        {
            let old = &mut project.state["steps"][id];
            if !step["scatter"].is_string() || old["status"] != "running" {
                continue;
            }
            let runs = old["run_ids"]
                .as_array()
                .context("running scatter has no run ids")?
                .clone();
            let mut results = old["results"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![Value::Null; runs.len()]);
            ensure!(
                runs.len() == results.len(),
                "scatter results/count mismatch"
            );
            for (n, run) in runs.iter().enumerate() {
                if results[n].is_object() {
                    continue;
                }
                let run = run.as_str().context("scatter run id is not a string")?;
                files::safe_component(run)?;
                let dir = src
                    .join("projects")
                    .join(&project.name)
                    .join("runs")
                    .join(run);
                if dir.join("exit.json").exists()
                    && files::json(&dir.join("exit.json"))?["code"] == 0
                {
                    ensure!(
                        dir.join("output.json").exists(),
                        "successful scatter item is missing outputs"
                    );
                    let output = files::json(&dir.join("output.json"))?;
                    ensure!(output.is_object(), "scatter result must be an object");
                    results[n] = output;
                }
            }
            old["results"] = json!(results);
        }
    }
    Ok(())
}

pub fn snapshot(
    old: &OldProject,
    plan: &Plan,
    rewrites: &[(String, String)],
) -> Result<StateSnapshot> {
    let mut state = StateSnapshot::default();
    let mut inputs = old.state.get("inputs").cloned().unwrap_or(json!({}));
    convert::rewrite_paths(&mut inputs, rewrites);
    state.inputs = serde_json::from_value(inputs)?;
    plan.validate_input_values(&state.inputs)
        .map_err(|errors| {
            anyhow::anyhow!(
                "{}",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })?;
    for (id, step) in plan.steps() {
        let entry = &old.state["steps"][id.as_str()];
        let mut outputs = entry["outputs"].clone();
        if outputs.is_null() {
            outputs = json!({});
        }
        convert::rewrite_paths(&mut outputs, rewrites);
        let outputs: JsonMap = serde_json::from_value(outputs)?;
        let status: StepStatus =
            serde_json::from_value(entry.get("status").cloned().unwrap_or(json!("pending")))?;
        if status == StepStatus::Succeeded {
            validate_outputs(step, &outputs, true, step.scatter.is_some())?;
        } else {
            validate_outputs(step, &outputs, false, step.scatter.is_some())?;
        }
        state.steps.insert(
            id.clone(),
            StepState {
                status: if status == StepStatus::Running {
                    StepStatus::Failed
                } else {
                    status
                },
                outputs,
                skipped: skip(entry)?,
                error: entry["error"].as_str().map(str::to_owned),
                ..Default::default()
            },
        );
    }
    for (id, step) in plan.steps() {
        let hash = sluice_model::plan::inputs_hash(plan, &state, step);
        state.steps.get_mut(id).expect("known step").inputs_hash = hash;
    }
    Ok(state)
}

pub fn validate_outputs(
    step: &sluice_model::Step,
    outputs: &JsonMap,
    required: bool,
    scatter: bool,
) -> Result<()> {
    for (name, value) in &outputs.0 {
        let ty = if scatter {
            step.output_type(name)
        } else {
            step.signature
                .outputs
                .get(name)
                .or_else(|| step.declared_outputs.get(name).map(|d| &d.ty))
                .cloned()
        }
        .with_context(|| format!("step {} has undeclared imported output {name}", step.id))?;
        check_value_at(&ty, value.as_value(), name).map_err(|e| {
            anyhow::anyhow!(
                "{}",
                e.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })?;
    }
    if required {
        for (name, ty) in step
            .signature
            .outputs
            .iter()
            .chain(step.declared_outputs.iter().map(|(n, d)| (n, &d.ty)))
        {
            if !matches!(ty, sluice_model::types::Type::Optional(_)) {
                ensure!(
                    outputs.0.contains_key(name),
                    "succeeded step {} missing output {name}",
                    step.id
                );
            }
        }
    }
    Ok(())
}

fn skip(entry: &Value) -> Result<Vec<SkipReason>> {
    let Some(reason) = entry["skipped"].as_str() else {
        return Ok(vec![]);
    };
    if let Some(id) = reason
        .strip_prefix("step ")
        .and_then(|s| s.strip_suffix(" was skipped"))
    {
        return Ok(vec![SkipReason::Step { step: id.parse()? }]);
    }
    if let Some((reference, value)) = reason.rsplit_once(" is ") {
        let value = match value {
            "false" => Some(false),
            "true" => Some(true),
            "null" => None,
            _ => anyhow::bail!("unrepresentable skipped reason: {reason}"),
        };
        return Ok(vec![SkipReason::Boolean {
            reference: reference.into(),
            value,
            negate: false,
        }]);
    }
    anyhow::bail!("unrepresentable skipped reason: {reason}")
}

pub fn effective(
    plan: &Plan,
    state: &StateSnapshot,
    step: &sluice_model::Step,
) -> Result<Option<Value>> {
    sluice_model::plan::effective_inputs(plan, state, step)
        .map(|inputs| {
            let mut out = json!({});
            for (name, input) in inputs {
                out[name] = match input {
                    EffectiveInput::File(path) => json!({"file":path}),
                    EffectiveInput::Value(v) => v.into_value(),
                };
            }
            Ok(out)
        })
        .transpose()
}

pub fn predecessor(
    src: &Path,
    old: &OldProject,
    step: &sluice_model::Step,
    index: i64,
    effective: &Value,
    rewrites: &[(String, String)],
) -> Result<Value> {
    let entry = &old.state["steps"][step.id.as_str()];
    let n = if index < 0 { 0 } else { index as usize };
    let source_run = entry["run_ids"].get(n).and_then(Value::as_str);
    let mut inputs = effective.clone();
    if let Some(scatter) = &step.scatter {
        inputs[scatter] = effective[scatter]
            .get(n)
            .context("scatter item not in effective input")?
            .clone();
    }
    let mut checkpoint = None;
    let mut outputs = entry
        .get("outputs")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or(json!({}));
    if index >= 0 {
        outputs = entry["results"]
            .get(n)
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(json!({}));
    }
    if let Some(run) = source_run {
        checkpoint = sessions::checkpoint(src, &old.name, run)?;
        if let Some(submitted) = old.submissions.get(run).and_then(Value::as_object) {
            for (name, value) in submitted {
                outputs[name] = value.clone();
            }
        }
        let input = src
            .join("projects")
            .join(&old.name)
            .join("runs")
            .join(run)
            .join("input.json");
        if input.exists() {
            inputs = files::json(&input)?;
        }
    }
    convert::rewrite_paths(&mut inputs, rewrites);
    convert::rewrite_paths(&mut outputs, rewrites);
    let _: JsonValue = inputs.clone().try_into()?;
    let _: JsonValue = outputs.clone().try_into()?;
    Ok(json!({"source_run":source_run,"inputs":inputs,"outputs":outputs,"checkpoint":checkpoint}))
}

pub fn provenance(src: &Path, old: &OldProject, step: &sluice_model::Step) -> Result<Value> {
    let mut facts = json!({});
    for run in old.state["steps"][step.id.as_str()]["run_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        files::safe_component(run)?;
        let input = src
            .join("projects")
            .join(&old.name)
            .join("runs")
            .join(run)
            .join("input.json");
        if !input.exists() {
            continue;
        }
        let input = files::json(&input)?;
        for (name, binding) in &step.bindings {
            if let sluice_model::Binding::File(path) = binding
                && let Some(bytes) = input[name].as_str()
            {
                facts[run][name] = json!({"file":path,"execution_bytes":sluice_model::hash::ExecutionProvenance::fingerprint(bytes.as_bytes()).to_string()});
            }
        }
    }
    Ok(facts)
}
