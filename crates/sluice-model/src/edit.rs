//! Convenience edits reduced to one validated patch and the plan core's simulation.

use crate::{
    commands::{
        self, CommandRequest, EditOptions, EditPreview, PatchOperation, StepSelection, StepStatus,
        UnsupportedInput,
    },
    error::PublicError,
    gates::{CachedResources, StateSnapshot},
    ids::{Revision, StepId, UnitName},
    plan::{self, Plan, ResourceLimit, SignatureProvider, Snapshot, ValidatedPlan, diagnostic},
    recipe::{ExpansionOptions, RecipeEntry, reserved_tags},
    rpc::{JsonMap, JsonValue},
    types::PathError,
    units::{PruneSet, prune_closed},
};
use indexmap::{IndexMap, IndexSet};
use serde_json::{Value, json};

/// Additional unit staging fields absent from P0's frozen transport.
#[derive(Debug, Clone, PartialEq)]
pub struct UnitAdd {
    pub request: commands::UnitAdd,
    pub tags: Vec<String>,
    pub inputs: IndexMap<String, JsonMap>,
}
impl From<commands::UnitAdd> for UnitAdd {
    fn from(request: commands::UnitAdd) -> Self {
        Self {
            request,
            tags: vec![],
            inputs: IndexMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanEdit {
    Patch(commands::PlanPatch),
    StepAdd(commands::StepAdd),
    UnitAdd(UnitAdd),
    StepUpdate(commands::StepUpdate),
    StepRemove(commands::StepRemove),
    EdgeAdd(commands::EdgeEdit),
    EdgeRemove(commands::EdgeEdit),
    StepSetInput(commands::StepSetInput),
    UnitTag(commands::UnitTag),
    StepPause(commands::StepPause),
    PlanPrune(commands::PlanPrune),
}
impl TryFrom<CommandRequest> for PlanEdit {
    type Error = PublicError;
    fn try_from(command: CommandRequest) -> Result<Self, PublicError> {
        Ok(match command {
            CommandRequest::PlanPatch(edit) => Self::Patch(edit),
            CommandRequest::StepAdd(edit) => Self::StepAdd(edit),
            CommandRequest::UnitAdd(edit) => Self::UnitAdd(edit.into()),
            CommandRequest::StepUpdate(edit) => Self::StepUpdate(edit),
            CommandRequest::StepRemove(edit) => Self::StepRemove(edit),
            CommandRequest::EdgeAdd(edit) => Self::EdgeAdd(edit),
            CommandRequest::EdgeRemove(edit) => Self::EdgeRemove(edit),
            CommandRequest::StepSetInput(edit) => Self::StepSetInput(edit),
            CommandRequest::UnitTag(edit) => Self::UnitTag(edit),
            CommandRequest::StepPause(edit) => Self::StepPause(edit),
            CommandRequest::PlanPrune(edit) => Self::PlanPrune(edit),
            _ => return Err(bad("command is not a plan edit")),
        })
    }
}
impl PlanEdit {
    fn options(&self) -> Option<&EditOptions> {
        Some(match self {
            Self::Patch(_) => return None,
            Self::StepAdd(e) => &e.edit,
            Self::UnitAdd(e) => &e.request.edit,
            Self::StepUpdate(e) => &e.edit,
            Self::StepRemove(e) => &e.edit,
            Self::EdgeAdd(e) | Self::EdgeRemove(e) => &e.edit,
            Self::StepSetInput(e) => &e.edit,
            Self::UnitTag(e) => &e.edit,
            Self::StepPause(e) => &e.edit,
            Self::PlanPrune(e) => &e.edit,
        })
    }
}

/// All inputs are immutable snapshots. For an age-filtered prune, the store must
/// supply the units whose last finish time meets that request's cutoff.
pub struct EditSnapshot<'a, P> {
    pub snapshot: &'a Snapshot,
    pub state: &'a StateSnapshot,
    pub signatures: &'a P,
    pub recipes: &'a IndexMap<String, RecipeEntry>,
    pub resources: &'a CachedResources,
    pub limits: &'a IndexMap<String, ResourceLimit>,
    pub prune_eligible: Option<&'a [UnitName]>,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputChanges {
    pub changed: Vec<StepId>,
    pub running: Vec<StepId>,
    pub unsupported: Vec<UnsupportedInput>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedEdit {
    pub expected: Revision,
    pub ops: Vec<PatchOperation>,
    pub plan: ValidatedPlan,
    pub preview: EditPreview,
    pub dry_run: bool,
    pub inputs: Option<InputChanges>,
    pub prune: Option<PruneSet>,
}
impl PreparedEdit {
    /// The store still owns the revision recheck and atomic commit.
    pub fn into_plan_edit(self) -> plan::PreparedEdit {
        plan::PreparedEdit {
            expected: self.expected,
            ops: self.ops,
            plan: self.plan,
            preview: self.preview,
        }
    }
}

/// Real provider-taking entry point. The provider-free P0 stub stays unchanged.
/// Preparation is pure for both dry runs and commits; dry runs return `preview`.
pub fn prepare_edit(
    context: &EditSnapshot<'_, impl SignatureProvider>,
    edit: PlanEdit,
) -> Result<PreparedEdit, PublicError> {
    let (expected, dry_run) = match &edit {
        PlanEdit::Patch(request) => (request.rev, request.dry_run),
        _ => {
            let options = edit.options().expect("convenience edit options");
            (
                options.expected.unwrap_or(context.snapshot.revision),
                options.dry_run,
            )
        }
    };
    if expected != context.snapshot.revision {
        return Err(PublicError::Conflict {
            message: "plan revision changed".into(),
            current_rev: Some(context.snapshot.revision),
        });
    }
    let plan = Plan::parse(&context.snapshot.document, context.signatures).map_err(invalid)?;
    let document = serde_json::to_value(&context.snapshot.document).expect("JSON serializes");
    let raw = document["steps"].as_object().expect("validated steps");
    let mut ops = vec![];
    let mut inputs_report = None;
    let mut prune_report = None;
    match edit {
        PlanEdit::Patch(request) => ops = request.ops,
        PlanEdit::StepAdd(request) => {
            if plan.steps().contains_key(&request.step) {
                return Err(bad(format!("step {} already exists", request.step)));
            }
            ops.push(add(
                &step_path(&request.step),
                serde_json::to_value(request.spec).expect("JSON serializes"),
            )?);
        }
        PlanEdit::UnitAdd(unit) => {
            let request = unit.request;
            let entry = context
                .recipes
                .get(&request.recipe)
                .ok_or_else(|| missing(format!("no recipe {}", request.recipe)))?;
            let recipe = entry.recipe.as_ref().map_err(|errors| {
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
            let mut params = request.params;
            if params
                .0
                .get("unit")
                .is_some_and(|v| v.as_value().as_str() != Some(request.unit.as_str()))
            {
                return Err(invalid(vec![diagnostic(
                    "params.unit",
                    "must match the requested unit",
                )]));
            }
            params
                .0
                .insert("unit".into(), JsonValue::try_from(json!(request.unit))?);
            let expanded = recipe
                .expand(
                    &params,
                    &ExpansionOptions {
                        start: request.start,
                        tags: unit.tags,
                        after: request.after,
                        inputs: unit.inputs,
                    },
                    &context.snapshot.document,
                    context.signatures,
                )
                .map_err(|errors| {
                    if errors
                        .iter()
                        .all(|error| error.message == "already exists in the plan")
                    {
                        bad(errors
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join("; "))
                    } else {
                        invalid(errors)
                    }
                })?;
            for (id, spec) in expanded.0 {
                ops.push(PatchOperation::Add {
                    path: format!("/steps/{}", pointer(&id)),
                    value: spec,
                });
            }
        }
        PlanEdit::StepUpdate(request) => {
            let mut step = raw_step(raw, &request.step)?.clone();
            if request.changes.0.is_empty() {
                return Err(bad("changes: expected at least one field"));
            }
            if request.changes.0.contains_key("when") {
                return Err(invalid(vec![diagnostic(
                    &format!("steps.{}.when", request.step),
                    "when is removed; use after entries",
                )]));
            }
            let fields = step.as_object_mut().expect("validated step");
            for (key, value) in request.changes.0 {
                if value.as_value().is_null() {
                    fields.shift_remove(&key);
                } else {
                    fields.insert(key, value.into_value());
                }
            }
            if &step != raw_step(raw, &request.step)? {
                ops.push(replace(&step_path(&request.step), step)?);
            }
        }
        PlanEdit::StepRemove(request) => {
            for id in select(&plan, &request.selection)? {
                ops.push(PatchOperation::Remove {
                    path: step_path(&id),
                });
            }
        }
        PlanEdit::EdgeAdd(request) => ops = edges(&plan, raw, &request, true)?,
        PlanEdit::EdgeRemove(request) => ops = edges(&plan, raw, &request, false)?,
        PlanEdit::StepSetInput(request) => {
            if request.inputs.0.is_empty() {
                return Err(bad("inputs: expected at least one input"));
            }
            let mut report = InputChanges::default();
            for id in select(&plan, &request.selection)? {
                if context.state.status(&id) == StepStatus::Running {
                    report.running.push(id);
                    continue;
                }
                let step = &plan.steps()[&id];
                let unsupported: Vec<_> = request
                    .inputs
                    .0
                    .keys()
                    .filter(|name| {
                        !step.signature.inputs.contains_key(*name)
                            && !step.bindings.contains_key(*name)
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
                let old = raw_step(raw, &id)?;
                let mut bindings = old.get("in").cloned().unwrap_or(json!({}));
                for (name, value) in &request.inputs.0 {
                    bindings
                        .as_object_mut()
                        .expect("validated bindings")
                        .insert(name.clone(), json!({"default":value}));
                }
                if old.get("in") != Some(&bindings) {
                    ops.push(set_field(old, &id, "in", Some(bindings))?);
                    report.changed.push(id);
                }
            }
            if report.changed.is_empty() {
                return Err(bad("step_set_input changes nothing"));
            }
            inputs_report = Some(report);
        }
        PlanEdit::UnitTag(request) => {
            let mut errors = reserved_tags(&request.add, "add");
            errors.extend(reserved_tags(&request.remove, "remove"));
            errors.extend(
                request
                    .add
                    .iter()
                    .filter(|tag| request.remove.contains(tag))
                    .map(|tag| diagnostic("add", format!("{tag} is removed too"))),
            );
            if !errors.is_empty() {
                return Err(invalid(errors));
            }
            let unit = plan
                .units()
                .get(&request.unit)
                .ok_or_else(|| missing(format!("no unit {}", request.unit)))?;
            for id in &unit.steps {
                let step = &plan.steps()[id];
                let tags: IndexSet<_> = step
                    .tags
                    .iter()
                    .filter(|tag| !request.remove.contains(tag))
                    .cloned()
                    .chain(request.add.iter().cloned())
                    .collect();
                if tags.iter().ne(step.tags.iter()) {
                    ops.push(set_field(
                        raw_step(raw, id)?,
                        id,
                        "tags",
                        Some(json!(tags)),
                    )?);
                }
            }
        }
        PlanEdit::StepPause(request) => {
            for id in select(&plan, &request.selection)? {
                let step = raw_step(raw, &id)?;
                let pause = if request.paused {
                    Some(Value::Bool(true))
                } else {
                    None
                };
                if step.get("paused") != pause.as_ref() {
                    ops.push(set_field(step, &id, "paused", pause)?);
                }
            }
        }
        PlanEdit::PlanPrune(request) => {
            if request.older_than_seconds > 0 && context.prune_eligible.is_none() {
                return Err(bad(
                    "age-filtered pruning requires store-supplied eligible units",
                ));
            }
            let selected = request.units.unwrap_or_else(|| {
                plan.units()
                    .iter()
                    .filter(|(_, unit)| unit.done(context.state))
                    .map(|(name, _)| name.clone())
                    .collect()
            });
            let unknown: Vec<_> = selected
                .iter()
                .filter(|name| !plan.units().contains_key(*name))
                .map(|name| diagnostic(&format!("units.{name}"), "no such unit"))
                .collect();
            if !unknown.is_empty() {
                return Err(invalid(unknown));
            }
            let selected: Vec<_> = selected
                .into_iter()
                .filter(|name| {
                    context
                        .prune_eligible
                        .is_none_or(|units| units.contains(name))
                })
                .collect();
            let closure = prune_closed(&plan, context.state, &selected).map_err(invalid)?;
            ops.extend(closure.steps.iter().map(|id| PatchOperation::Remove {
                path: step_path(id),
            }));
            prune_report = Some(closure);
        }
    }
    let prepared = plan::prepare_plan_edit(
        context.snapshot,
        context.state,
        plan::PlanEdit { expected, ops },
        context.signatures,
        context.resources,
        context.limits,
    )?;
    Ok(PreparedEdit {
        expected: prepared.expected,
        ops: prepared.ops,
        plan: prepared.plan,
        preview: prepared.preview,
        dry_run,
        inputs: inputs_report,
        prune: prune_report,
    })
}

fn edges(
    plan: &Plan,
    raw: &serde_json::Map<String, Value>,
    request: &commands::EdgeEdit,
    adding: bool,
) -> Result<Vec<PatchOperation>, PublicError> {
    if request.after.is_empty() {
        return Err(bad("after: name at least one gate entry"));
    }
    let mut errors = vec![];
    let targets = if let Some(name) = request.step.strip_prefix("unit:") {
        let name = UnitName::new(name).map_err(|error| bad(error.to_string()))?;
        plan.units()
            .get(&name)
            .map(|u| u.entries.clone())
            .unwrap_or_else(|| {
                errors.push(diagnostic(&format!("units.{name}"), "no such unit"));
                vec![]
            })
    } else {
        let id = StepId::new(&request.step).map_err(|error| bad(error.to_string()))?;
        if !plan.steps().contains_key(&id) {
            errors.push(diagnostic(&format!("steps.{id}"), "no such step"));
            vec![]
        } else {
            vec![id]
        }
    };
    for entry in &request.after {
        match crate::gates::Gate::compile(entry, plan) {
            Ok(crate::gates::Gate::Unit { name, .. }) if !plan.units().contains_key(&name) => {
                errors.push(diagnostic("after", format!("no unit {name}")));
            }
            Err(error) => errors.push(diagnostic("after", error)),
            _ => {}
        }
    }
    if !errors.is_empty() {
        return Err(invalid(errors));
    }
    let mut ops = vec![];
    for id in targets {
        let step = raw_step(raw, &id)?;
        let old: Vec<_> = step
            .get("after")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|v| v.as_str().expect("validated gate").to_owned())
            .collect();
        let new: Vec<_> = if adding {
            old.iter()
                .cloned()
                .chain(request.after.iter().cloned())
                .collect::<IndexSet<_>>()
                .into_iter()
                .collect()
        } else {
            old.iter()
                .filter(|e| !request.after.contains(e))
                .cloned()
                .collect()
        };
        if old != new {
            ops.push(set_field(
                step,
                &id,
                "after",
                if new.is_empty() {
                    None
                } else {
                    Some(json!(new))
                },
            )?);
        }
    }
    Ok(ops)
}
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
    Ok(plan
        .steps()
        .iter()
        .filter(|(id, step)| steps.contains(id) || step.tags.iter().any(|tag| tags.contains(tag)))
        .map(|(id, _)| id.clone())
        .collect())
}
fn raw_step<'a>(
    raw: &'a serde_json::Map<String, Value>,
    id: &StepId,
) -> Result<&'a Value, PublicError> {
    raw.get(id.as_str())
        .ok_or_else(|| missing(format!("no step {id}")))
}
fn pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}
fn step_path(id: &StepId) -> String {
    format!("/steps/{}", pointer(id.as_str()))
}
fn add(path: &str, value: Value) -> Result<PatchOperation, PublicError> {
    Ok(PatchOperation::Add {
        path: path.into(),
        value: JsonValue::try_from(value)?,
    })
}
fn replace(path: &str, value: Value) -> Result<PatchOperation, PublicError> {
    Ok(PatchOperation::Replace {
        path: path.into(),
        value: JsonValue::try_from(value)?,
    })
}
fn set_field(
    step: &Value,
    id: &StepId,
    field: &str,
    value: Option<Value>,
) -> Result<PatchOperation, PublicError> {
    let path = format!("{}/{}", step_path(id), pointer(field));
    match value {
        Some(value) if step.get(field).is_some() => replace(&path, value),
        Some(value) => add(&path, value),
        None => Ok(PatchOperation::Remove { path }),
    }
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
