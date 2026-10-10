//! The typed edit tools lowered to `plan_edit`'s operations (`docs/design/plan-rows.md` §7.8).
//!
//! Each tool resolves its own arguments against the certified base (selections by `steps` and
//! `tags`, a unit's members) and becomes a list of `PlanOp`s for the one pipeline
//! (`plan::prepare_plan_edit`, through `prepare_lowered`). It keeps its own refusals and their
//! kinds (`not_found` for an unknown step or unit, `bad_request` for an existing one), which it
//! reports before any operation is prepared. Lowering may produce no operation at all (an edge
//! already there, tags or pauses as asked, a prune whose closure is empty): the edit then
//! changes nothing and commits nothing. Lowering never builds an operation with an empty list
//! or empty changes.

use crate::{
    commands::{CommandRequest, StepSelection, StepStatus, UnsupportedInput},
    error::PublicError,
    gates::{Gate, StateSnapshot},
    ids::{StepId, UnitName},
    plan::{EditBase, Plan, PrepareOptions, SignatureProvider, diagnostic, prepare_plan_edit},
    plan_rows::{PlanChange, PlanOp, PreparedPlanEdit, StepChanges},
    recipe::{ExpansionOptions, RecipeEntry, reserved_tags},
    rpc::{JsonMap, JsonValue},
    types::PathError,
    units::{PruneSet, prune_closed},
};
use indexmap::{IndexMap, IndexSet};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

/// A plan edit command's name and author, for the coordinator's log; None for any other
/// command.
pub fn edit_label(command: &CommandRequest) -> Option<(&'static str, Option<&str>)> {
    let (kind, options) = match command {
        CommandRequest::PlanPatch(e) => return Some(("plan_patch", e.author.as_deref())),
        CommandRequest::StepAdd(e) => ("step_add", &e.edit),
        CommandRequest::UnitAdd(e) => ("unit_add", &e.edit),
        CommandRequest::StepUpdate(e) => ("step_update", &e.edit),
        CommandRequest::StepRemove(e) => ("step_remove", &e.edit),
        CommandRequest::EdgeAdd(e) => ("edge_add", &e.edit),
        CommandRequest::EdgeRemove(e) => ("edge_remove", &e.edit),
        CommandRequest::StepSetInput(e) => ("step_set_input", &e.edit),
        CommandRequest::PlanSetInput(e) => ("plan_set_input", &e.edit),
        CommandRequest::UnitTag(e) => ("unit_tag", &e.edit),
        CommandRequest::StepPause(e) => ("step_pause", &e.edit),
        CommandRequest::PlanPrune(e) => ("plan_prune", &e.edit),
        _ => return None,
    };
    Some((kind, options.author.as_deref()))
}

/// `step_set_input`'s report.
#[derive(Debug, Clone, Default, PartialEq, Serialize, JsonSchema)]
pub struct InputChanges {
    pub changed: Vec<StepId>,
    pub running: Vec<StepId>,
    pub unsupported: Vec<UnsupportedInput>,
}

/// What the reply's `steps` are for a lowered tool.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplySteps {
    /// The ids the edit's `step.add` and `unit.add` operations add (`step_add`, `unit_add`).
    Added,
    /// These steps, as the tool resolved them (`step_pause`, `unit_tag`, `unit_remove`,
    /// `plan_prune`, `step_remove`).
    These(Vec<StepId>),
    /// The steps among these whose rows the edit puts, in plan order (`unit_update`).
    Changed(IndexSet<StepId>),
    /// None (`step_update`, `edge_add`, `edge_remove`, `step_set_input`).
    None,
}

/// A typed tool's operations and the parts of its reply it keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct Lowered {
    pub ops: Vec<PlanOp>,
    pub steps: ReplySteps,
    /// `step_set_input`'s report.
    pub inputs: Option<InputChanges>,
    /// `plan_prune`'s removal set (the store checks its age evidence beside it).
    pub prune: Option<PruneSet>,
}
impl Lowered {
    fn ops(ops: Vec<PlanOp>, steps: ReplySteps) -> Self {
        Self {
            ops,
            steps,
            inputs: None,
            prune: None,
        }
    }
}

/// Prepare a lowered tool through the one pipeline and give it its own reply parts.
pub fn prepare_lowered(
    base: &EditBase<'_, impl SignatureProvider>,
    lowered: Lowered,
    options: PrepareOptions,
) -> Result<PreparedPlanEdit, PublicError> {
    let Lowered {
        ops,
        steps,
        inputs,
        prune,
    } = lowered;
    let mut prepared = prepare_plan_edit(base, ops, options)?;
    prepared.steps = match steps {
        ReplySteps::Added => prepared.steps,
        ReplySteps::These(steps) => Some(steps),
        ReplySteps::Changed(members) => Some(
            prepared
                .commit
                .rows
                .changes
                .iter()
                .filter_map(|change| match change {
                    PlanChange::StepPut { step, .. } if members.contains(step) => {
                        Some(step.clone())
                    }
                    _ => None,
                })
                .collect(),
        ),
        ReplySteps::None => None,
    };
    prepared.inputs = inputs;
    prepared.commit.prune = prune;
    Ok(prepared)
}

/// `step_add`: one `step.add`; an existing id is the tool's own `bad_request`. Its `start`
/// goes in the options.
pub fn step_add(plan: &Plan, step: &StepId, spec: &JsonMap) -> Result<Lowered, PublicError> {
    if plan.steps().contains_key(step) {
        return Err(bad(format!("step {step} already exists")));
    }
    Ok(Lowered::ops(
        vec![PlanOp::StepAdd {
            step: step.clone(),
            spec: spec.clone(),
        }],
        ReplySteps::Added,
    ))
}

/// `step_update`: one `step.update`; an unknown step is `not_found`. Changes that leave the
/// declaration's data as it is commit nothing.
pub fn step_update(
    plan: &Plan,
    step: &StepId,
    changes: &StepChanges,
) -> Result<Lowered, PublicError> {
    if !plan.steps().contains_key(step) {
        return Err(missing(format!("no step {step}")));
    }
    if changes.is_empty() {
        return Err(bad("changes: expected at least one field"));
    }
    Ok(Lowered::ops(
        vec![PlanOp::StepUpdate {
            step: step.clone(),
            changes: Box::new(changes.clone()),
        }],
        ReplySteps::None,
    ))
}

/// `step_remove`: one `step.remove` of the selection; none when the selection is empty.
pub fn step_remove(plan: &Plan, selection: &StepSelection) -> Result<Lowered, PublicError> {
    let steps = select(plan, selection)?;
    let ops = if steps.is_empty() {
        vec![]
    } else {
        vec![PlanOp::StepRemove {
            steps: steps.clone(),
        }]
    };
    Ok(Lowered::ops(ops, ReplySteps::These(steps)))
}

/// `edge_add` and `edge_remove`: the target (a step, or `unit:<u>` for its entry steps) and
/// each entry checked against the base as the tool always has (`invalid`), then one operation
/// with the entries not already there (for `edge_remove`: those there); none when that leaves
/// no entry.
pub fn edge(
    plan: &Plan,
    step: &str,
    after: &[String],
    adding: bool,
) -> Result<Lowered, PublicError> {
    if after.is_empty() {
        return Err(bad("after: name at least one gate entry"));
    }
    let mut errors = vec![];
    let targets = if let Some(name) = step.strip_prefix("unit:") {
        let name = UnitName::new(name).map_err(|error| bad(error.to_string()))?;
        plan.units()
            .get(&name)
            .map(|u| u.entries.clone())
            .unwrap_or_else(|| {
                errors.push(diagnostic(&format!("units.{name}"), "no such unit"));
                vec![]
            })
    } else {
        let id = StepId::new(step).map_err(|error| bad(error.to_string()))?;
        if !plan.steps().contains_key(&id) {
            errors.push(diagnostic(&format!("steps.{id}"), "no such step"));
            vec![]
        } else {
            vec![id]
        }
    };
    for entry in after {
        match Gate::compile(entry, plan) {
            Ok(Gate::Unit { name, .. }) if !plan.units().contains_key(&name) => {
                errors.push(diagnostic("after", format!("no unit {name}")));
            }
            Err(error) => errors.push(diagnostic("after", error)),
            _ => {}
        }
    }
    if !errors.is_empty() {
        return Err(invalid(errors));
    }
    let present = |entry: &String, id: &StepId| {
        plan.steps()[id]
            .declaration
            .0
            .get("after")
            .and_then(|a| a.as_value().as_array())
            .is_some_and(|old| old.iter().any(|e| e.as_str() == Some(entry.as_str())))
    };
    let entries: Vec<String> = after
        .iter()
        .collect::<IndexSet<_>>()
        .into_iter()
        .filter(|entry| {
            targets.iter().any(|id| {
                if adding {
                    !present(entry, id)
                } else {
                    present(entry, id)
                }
            })
        })
        .cloned()
        .collect();
    let ops = match (entries.is_empty(), adding) {
        (true, _) => vec![],
        (false, true) => vec![PlanOp::EdgeAdd {
            step: step.to_owned(),
            after: entries,
        }],
        (false, false) => vec![PlanOp::EdgeRemove {
            step: step.to_owned(),
            after: entries,
        }],
    };
    Ok(Lowered::ops(ops, ReplySteps::None))
}

/// `unit_add`: one `unit.add`, its expansion checked against the base first so the tool keeps
/// its own refusals: an unknown recipe is `not_found`, ids already in the plan `bad_request`,
/// a broken recipe or bad params `invalid`. Its `start` goes in the options.
#[allow(clippy::too_many_arguments)]
pub fn unit_add(
    plan: &Plan,
    recipes: &IndexMap<String, RecipeEntry>,
    signatures: &impl SignatureProvider,
    recipe: &str,
    unit: &UnitName,
    params: &JsonMap,
    after: &IndexMap<String, Vec<String>>,
    inputs: &IndexMap<String, JsonMap>,
    tags: &[String],
) -> Result<Lowered, PublicError> {
    let entry = recipes
        .get(recipe)
        .ok_or_else(|| missing(format!("no recipe {recipe}")))?;
    let found = entry.recipe.as_ref().map_err(|errors| {
        invalid(
            errors
                .iter()
                .map(|e| {
                    diagnostic(
                        &e.path,
                        format!("recipe {} ({}): {}", entry.name, entry.scope, e.message),
                    )
                })
                .collect(),
        )
    })?;
    if params
        .0
        .get("unit")
        .is_some_and(|v| v.as_value().as_str() != Some(unit.as_str()))
    {
        return Err(invalid(vec![diagnostic(
            "params.unit",
            "must match the requested unit",
        )]));
    }
    let mut full = params.clone();
    full.0
        .insert("unit".into(), JsonValue::try_from(json!(unit))?);
    let options = ExpansionOptions {
        start: true,
        tags: tags.to_vec(),
        after: after.clone(),
        inputs: inputs.clone(),
    };
    let (steps, _) = found.stage(&full, &options, signatures).map_err(invalid)?;
    let collisions: Vec<_> = steps
        .keys()
        .filter(|id| StepId::new(*id).is_ok_and(|id| plan.steps().contains_key(&id)))
        .map(|id| diagnostic(&format!("steps.{id}"), "already exists in the plan").to_string())
        .collect();
    if !collisions.is_empty() {
        return Err(bad(collisions.join("; ")));
    }
    Ok(Lowered::ops(
        vec![PlanOp::UnitAdd {
            recipe: recipe.to_owned(),
            unit: unit.clone(),
            params: params.clone(),
            after: after.clone(),
            inputs: inputs.clone(),
            tags: tags.to_vec(),
        }],
        ReplySteps::Added,
    ))
}

/// `step_set_input`: one `step.update {in}` per selected step that is not running, takes
/// every input and whose bindings change; its report kept. `status` is each selected step's
/// stored status. No step changed is its own `bad_request`.
pub fn step_set_input(
    plan: &Plan,
    status: &dyn Fn(&StepId) -> StepStatus,
    selection: &StepSelection,
    inputs: &JsonMap,
) -> Result<Lowered, PublicError> {
    if inputs.0.is_empty() {
        return Err(bad("inputs: expected at least one input"));
    }
    let mut report = InputChanges::default();
    let mut ops = vec![];
    for id in select(plan, selection)? {
        if status(&id) == StepStatus::Running {
            report.running.push(id);
            continue;
        }
        let step = &plan.steps()[&id];
        let unsupported: Vec<_> = inputs
            .0
            .keys()
            .filter(|name| {
                !step.signature.inputs.contains_key(*name) && !step.bindings.contains_key(*name)
            })
            .cloned()
            .collect();
        if !unsupported.is_empty() {
            report.unsupported.push(UnsupportedInput {
                step: id,
                inputs: unsupported,
            });
            continue;
        }
        let old = step
            .declaration
            .0
            .get("in")
            .map(|bindings| bindings.as_value().clone());
        let mut bindings = old.clone().unwrap_or(json!({}));
        for (name, value) in &inputs.0 {
            bindings
                .as_object_mut()
                .ok_or_else(|| bad(format!("steps.{id}.in is not an object")))?
                .insert(name.clone(), json!({"default": value}));
        }
        if !old
            .map(|old| crate::hash::data_equal(&old, &bindings))
            .transpose()?
            .unwrap_or(false)
        {
            ops.push(PlanOp::StepUpdate {
                step: id.clone(),
                changes: Box::new(StepChanges {
                    bindings: Some(Some(JsonValue::try_from(bindings)?)),
                    ..StepChanges::default()
                }),
            });
            report.changed.push(id);
        }
    }
    if report.changed.is_empty() {
        return Err(bad("step_set_input changes nothing"));
    }
    Ok(Lowered {
        ops,
        steps: ReplySteps::None,
        inputs: Some(report),
        prune: None,
    })
}

/// `unit_tag`: one `step.update {tags}` per member whose tags change; none when none does.
pub fn unit_tag(
    plan: &Plan,
    unit: &UnitName,
    add: &[String],
    remove: &[String],
) -> Result<Lowered, PublicError> {
    let mut errors = reserved_tags(add, "add");
    errors.extend(reserved_tags(remove, "remove"));
    errors.extend(
        add.iter()
            .filter(|tag| remove.contains(tag))
            .map(|tag| diagnostic("add", format!("{tag} is removed too"))),
    );
    if !errors.is_empty() {
        return Err(invalid(errors));
    }
    let members = &plan
        .units()
        .get(unit)
        .ok_or_else(|| missing(format!("no unit {unit}")))?
        .steps;
    let mut ops = vec![];
    for id in members {
        let step = &plan.steps()[id];
        let tags: IndexSet<_> = step
            .tags
            .iter()
            .filter(|tag| !remove.contains(tag))
            .cloned()
            .chain(add.iter().cloned())
            .collect();
        if tags.iter().ne(step.tags.iter()) {
            ops.push(PlanOp::StepUpdate {
                step: id.clone(),
                changes: Box::new(StepChanges {
                    tags: Some(Some(JsonValue::try_from(json!(tags))?)),
                    ..StepChanges::default()
                }),
            });
        }
    }
    Ok(Lowered::ops(ops, ReplySteps::These(members.clone())))
}

/// `step_pause`: one `step.update {paused}` per selected step (and, with `subtree`, every step
/// downstream of one) whose pause changes. Pausing keeps the reason on each step it pauses;
/// without one, `true`, and a step already paused keeps its own reason.
pub fn step_pause(
    plan: &Plan,
    selection: &StepSelection,
    subtree: bool,
    paused: bool,
    reason: &str,
) -> Result<Lowered, PublicError> {
    let mut chosen = select(plan, selection)?;
    if subtree {
        chosen = downstream(plan, &chosen);
    }
    let reason = reason.trim();
    let mut ops = vec![];
    for id in &chosen {
        let step = &plan.steps()[id];
        let pause = match (paused, reason.is_empty()) {
            (false, _) => None,
            (true, true) if step.paused.is_paused() => continue,
            (true, true) => Some(Value::Bool(true)),
            (true, false) => Some(Value::String(reason.to_owned())),
        };
        if step.declaration.0.get("paused").map(JsonValue::as_value) != pause.as_ref() {
            ops.push(PlanOp::StepUpdate {
                step: id.clone(),
                changes: Box::new(StepChanges {
                    paused: Some(pause.map(JsonValue::try_from).transpose()?),
                    ..StepChanges::default()
                }),
            });
        }
    }
    Ok(Lowered::ops(ops, ReplySteps::These(chosen)))
}

/// `plan_prune`: one `step.remove` of the closure of the selected done units when it is not
/// empty, else nothing; its report kept. `state` holds the stored state of every step of the
/// units it considers; `eligible`, for an age-filtered prune, the units the store's evidence
/// finds old enough.
#[allow(clippy::too_many_arguments)]
pub fn plan_prune(
    plan: &Plan,
    state: &StateSnapshot,
    units: Option<&[UnitName]>,
    tags: Option<&[String]>,
    older_than_seconds: u64,
    keep: Option<&[String]>,
    eligible: Option<&[UnitName]>,
) -> Result<Lowered, PublicError> {
    if older_than_seconds > 0 && eligible.is_none() {
        return Err(bad(
            "age-filtered pruning requires store-supplied eligible units",
        ));
    }
    let has_selection = units.is_some() || tags.is_some();
    let explicit = units.unwrap_or_default();
    let unknown: Vec<_> = explicit
        .iter()
        .filter(|name| !plan.units().contains_key(*name))
        .map(|name| diagnostic(&format!("units.{name}"), "no such unit"))
        .collect();
    if !unknown.is_empty() {
        return Err(invalid(unknown));
    }
    let selected: Vec<UnitName> = plan
        .units()
        .iter()
        .filter(|(name, unit)| {
            if !has_selection {
                return unit.done(state);
            }
            explicit.contains(name)
                || (unit.done(state)
                    && tags.is_some_and(|tags| {
                        unit.steps
                            .iter()
                            .any(|id| plan.steps()[id].tags.iter().any(|tag| tags.contains(tag)))
                    }))
        })
        .map(|(name, _)| name.clone())
        .filter(|name| eligible.is_none_or(|units| units.contains(name)))
        .collect();
    let keep = keep.unwrap_or_default();
    crate::units::check_keep("keep", keep).map_err(invalid)?;
    let closure = prune_closed(plan, state, &selected, keep).map_err(invalid)?;
    let ops = if closure.steps.is_empty() {
        vec![]
    } else {
        vec![PlanOp::StepRemove {
            steps: closure.steps.clone(),
        }]
    };
    Ok(Lowered {
        ops,
        steps: ReplySteps::These(closure.steps.clone()),
        inputs: None,
        prune: Some(closure),
    })
}

/// `unit_update`: one `unit.update`; an unknown unit is `not_found` (`no unit <name>`). Its
/// reply's steps are the members it changes.
pub fn unit_update(
    plan: &Plan,
    unit: &UnitName,
    changes: &IndexMap<StepId, StepChanges>,
) -> Result<Lowered, PublicError> {
    if !plan.units().contains_key(unit) {
        return Err(crate::plan_rows::PlanRowsError::NoUnit { unit: unit.clone() }.into());
    }
    Ok(Lowered::ops(
        vec![PlanOp::UnitUpdate {
            unit: unit.clone(),
            changes: changes.clone(),
        }],
        ReplySteps::Changed(changes.keys().cloned().collect()),
    ))
}

/// `unit_remove`: one `unit.remove`; an unknown unit is `not_found` (`no unit <name>`). Its
/// reply's steps are the members it removes, in plan order.
pub fn unit_remove(plan: &Plan, unit: &UnitName) -> Result<Lowered, PublicError> {
    let members = plan
        .units()
        .get(unit)
        .ok_or_else(|| crate::plan_rows::PlanRowsError::NoUnit { unit: unit.clone() })?
        .steps
        .clone();
    Ok(Lowered::ops(
        vec![PlanOp::UnitRemove { unit: unit.clone() }],
        ReplySteps::These(members),
    ))
}

/// The selected steps and every step downstream of them (reading from or gated on one,
/// transitively), in plan order.
fn downstream(plan: &Plan, selected: &[StepId]) -> Vec<StepId> {
    let mut found: IndexSet<StepId> = selected.iter().cloned().collect();
    let mut index = 0;
    while index < found.len() {
        let id = found[index].clone();
        index += 1;
        found.extend(plan.dependents(&id));
    }
    let mut found: Vec<(u64, StepId)> = found
        .into_iter()
        .filter_map(|id| Some((plan.position(&id)?, id)))
        .collect();
    found.sort();
    found.into_iter().map(|(_, id)| id).collect()
}

/// The steps a selection names (by id or by tag), in plan order. An unknown id is `not_found`.
fn select(plan: &Plan, selection: &StepSelection) -> Result<Vec<StepId>, PublicError> {
    let steps = selection.steps.as_deref().unwrap_or_default();
    let tags = selection.tags.as_deref().unwrap_or_default();
    if steps.is_empty() && tags.is_empty() {
        return Err(bad("select steps by steps and/or tags"));
    }
    let unknown: Vec<_> = steps
        .iter()
        .filter(|id| !plan.steps().contains_key(*id))
        .map(ToString::to_string)
        .collect();
    if !unknown.is_empty() {
        return Err(missing(format!("no steps {}", unknown.join(", "))));
    }
    let mut chosen: IndexSet<&StepId> = steps.iter().collect();
    for tag in tags {
        chosen.extend(plan.tagged(tag));
    }
    let mut chosen: Vec<(u64, StepId)> = chosen
        .into_iter()
        .filter_map(|id| Some((plan.position(id)?, id.clone())))
        .collect();
    chosen.sort();
    Ok(chosen.into_iter().map(|(_, id)| id).collect())
}

fn bad(message: impl Into<String>) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}
fn missing(message: impl Into<String>) -> PublicError {
    PublicError::NotFound {
        message: message.into(),
    }
}
fn invalid(errors: Vec<PathError>) -> PublicError {
    PublicError::Invalid {
        message: "invalid plan edit".into(),
        errors: errors.iter().map(ToString::to_string).collect(),
    }
}
