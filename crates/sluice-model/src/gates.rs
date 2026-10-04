//! Shared admission and skip evaluator, state reconciliation and edit simulation.

use crate::{
    commands::StepStatus,
    hash::InputsHash,
    ids::{StepId, UnitName},
    plan::{Binding, Pause, Plan, Step, diagnostic, inputs_hash},
    rpc::{JsonMap, JsonValue},
    types::{BoundValue, PathError, SkipReason, Type, navigate_value},
};
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ValueRef(pub String);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub step: Option<StepId>,
    pub name: String,
    pub fields: Vec<String>,
}
impl fmt::Display for ValueRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl ValueRef {
    pub fn parse(text: &str) -> Result<Self, String> {
        let reference = Self(text.into());
        reference.parts()?;
        Ok(reference)
    }
    pub fn parts(&self) -> Result<Reference, String> {
        let mut segments = self.0.split('.');
        let head = segments.next().unwrap_or_default();
        let (step, name) = match head.split_once('/') {
            Some((step, name)) => (Some(StepId::new(step).map_err(|_| self.bad_ref())?), name),
            None => (None, head),
        };
        StepId::new(name).map_err(|_| self.bad_ref())?;
        let fields: Vec<_> = segments.map(str::to_owned).collect();
        if fields
            .iter()
            .any(|field| field.is_empty() || field.contains('/'))
        {
            return Err(self.bad_ref());
        }
        Ok(Reference {
            step,
            name: name.into(),
            fields,
        })
    }
    fn bad_ref(&self) -> String {
        format!(
            "bad ref {:?}: expected <input> or <step>/<output>, then .<field>...",
            self.0
        )
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Gate {
    Step { id: StepId, accept_skip: bool },
    Bool { reference: ValueRef, negate: bool },
    Unit { name: UnitName, accept_skip: bool },
}
impl Gate {
    /// Resolve bare entries using the shared input/step namespace. Unit existence
    /// is checked after deriving the complete unit graph.
    pub fn compile(text: &str, plan: &Plan) -> Result<Self, String> {
        let (negate, entry) = match text.strip_prefix('!') {
            Some(entry) => (true, entry),
            None => (false, text),
        };
        let (accept_skip, entry) = match entry.strip_suffix('?') {
            Some(entry) => (true, entry),
            None => (false, entry),
        };
        if let Some(name) = entry.strip_prefix("unit:") {
            if negate {
                return Err("! is only allowed on boolean refs, not unit entries".into());
            }
            return Ok(Self::Unit {
                name: UnitName::new(name).map_err(|error| error.to_string())?,
                accept_skip,
            });
        }
        if let Ok(id) = StepId::new(entry)
            && plan.steps().contains_key(&id)
        {
            if negate {
                return Err("! is only allowed on boolean refs, not step entries".into());
            }
            return Ok(Self::Step { id, accept_skip });
        }
        if accept_skip {
            return Err("? is only allowed on step and unit entries, not refs".into());
        }
        let reference = ValueRef::parse(entry)?;
        let ty = plan.reference_type(&reference)?;
        let mut inner = &ty;
        while let Type::Optional(ty) = inner {
            inner = ty;
        }
        if !matches!(inner, Type::Boolean | Type::Any) {
            return Err(format!("{reference} is {ty}, not a boolean"));
        }
        Ok(Self::Bool { reference, negate })
    }
    pub fn entry(&self) -> String {
        match self {
            Self::Step { id, accept_skip } => {
                format!("{id}{}", if *accept_skip { "?" } else { "" })
            }
            Self::Unit { name, accept_skip } => {
                format!("unit:{name}{}", if *accept_skip { "?" } else { "" })
            }
            Self::Bool { reference, negate } => {
                format!("{}{reference}", if *negate { "!" } else { "" })
            }
        }
    }
}
impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boolean {
                reference, value, ..
            } => write!(
                f,
                "{reference} is {}",
                match value {
                    None => "null",
                    Some(true) => "true",
                    Some(false) => "false",
                }
            ),
            Self::Step { step } => write!(f, "step {step} was skipped"),
            Self::Unit { unit, exit } => write!(f, "unit {unit} was skipped (exit step {exit})"),
        }
    }
}

/// A projection of durable state, with no run metadata or process ownership.
#[derive(Debug, Clone, PartialEq)]
pub struct StepState {
    pub status: StepStatus,
    pub outputs: JsonMap,
    pub inputs_hash: Option<InputsHash>,
    pub skipped: Vec<SkipReason>,
    pub error: Option<String>,
    pub queued: Vec<String>,
}
impl Default for StepState {
    fn default() -> Self {
        Self {
            status: StepStatus::Pending,
            outputs: JsonMap::default(),
            inputs_hash: None,
            skipped: vec![],
            error: None,
            queued: vec![],
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StateSnapshot {
    pub inputs: JsonMap,
    pub steps: IndexMap<StepId, StepState>,
    pub paused: Pause,
}
impl StateSnapshot {
    pub fn status(&self, id: &StepId) -> StepStatus {
        self.steps
            .get(id)
            .map(|state| state.status.clone())
            .unwrap_or(StepStatus::Pending)
    }
}

/// Entries are evaluated completely, in order. All invalid diagnostics remain
/// observable, even if another entry requests a skip or is still waiting.
#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    Ready,
    Wait(Vec<String>),
    Skip(Vec<SkipReason>),
    Invalid(Vec<PathError>),
}
impl GateDecision {
    fn combine(decisions: impl IntoIterator<Item = Self>) -> Self {
        let mut invalid = vec![];
        let mut skipped = vec![];
        let mut waiting = vec![];
        for decision in decisions {
            match decision {
                Self::Ready => {}
                Self::Wait(reasons) => {
                    for reason in reasons {
                        if !waiting.contains(&reason) {
                            waiting.push(reason);
                        }
                    }
                }
                Self::Skip(reasons) => {
                    for reason in reasons {
                        if !skipped.contains(&reason) {
                            skipped.push(reason);
                        }
                    }
                }
                Self::Invalid(errors) => invalid.extend(errors),
            }
        }
        if !invalid.is_empty() {
            Self::Invalid(invalid)
        } else if !skipped.is_empty() {
            Self::Skip(skipped)
        } else if !waiting.is_empty() {
            Self::Wait(waiting)
        } else {
            Self::Ready
        }
    }
}
fn status_name(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Succeeded => "succeeded",
        StepStatus::Failed => "failed",
        StepStatus::Stale => "stale",
        StepStatus::Skipped => "skipped",
    }
}

/// Resolve root availability before navigation. An absent required root waits;
/// an absent optional root and missing paths within available values are null.
pub fn resolve_reference(plan: &Plan, state: &StateSnapshot, reference: &ValueRef) -> BoundValue {
    let Ok(parts) = reference.parts() else {
        return BoundValue::Waiting;
    };
    let (base, root_type) = if let Some(id) = parts.step {
        match state.status(&id) {
            StepStatus::Skipped => return BoundValue::Skipped(SkipReason::Step { step: id }),
            StepStatus::Succeeded => (
                state
                    .steps
                    .get(&id)
                    .and_then(|entry| entry.outputs.0.get(&parts.name)),
                plan.steps()
                    .get(&id)
                    .and_then(|step| step.output_type(&parts.name)),
            ),
            _ => return BoundValue::Waiting,
        }
    } else {
        (
            state.inputs.0.get(&parts.name),
            plan.inputs().get(&parts.name).map(|decl| decl.ty.clone()),
        )
    };
    let null = || BoundValue::Ready(JsonValue::try_from(Value::Null).expect("null"));
    let Some(base) = base else {
        return if matches!(root_type, Some(Type::Optional(_))) {
            null()
        } else {
            BoundValue::Waiting
        };
    };
    let fields: Vec<_> = parts.fields.iter().map(String::as_str).collect();
    match navigate_value(base.as_value(), &fields) {
        Some(value) => {
            BoundValue::Ready(JsonValue::try_from(value.clone()).expect("strict snapshot value"))
        }
        None => null(),
    }
}
fn wait_reference(reference: &ValueRef, state: &StateSnapshot) -> String {
    match reference.parts() {
        Ok(Reference { step: Some(id), .. }) if state.status(&id) != StepStatus::Succeeded => {
            format!("step {id} is {}", status_name(&state.status(&id)))
        }
        Ok(Reference { step: Some(_), .. }) => format!("{reference} has no value"),
        Ok(Reference { name, .. }) => format!("plan input {name} has no value"),
        Err(error) => error,
    }
}
pub fn evaluate_gate(plan: &Plan, state: &StateSnapshot, gate: &Gate) -> GateDecision {
    match gate {
        Gate::Step { id, .. } if !plan.steps().contains_key(id) => {
            return GateDecision::Invalid(vec![diagnostic("after", format!("no step {id}"))]);
        }
        Gate::Bool { reference, .. } => {
            let ty = match plan.reference_type(reference) {
                Ok(ty) => ty,
                Err(error) => return GateDecision::Invalid(vec![diagnostic("after", error)]),
            };
            let mut inner = &ty;
            while let Type::Optional(ty) = inner {
                inner = ty;
            }
            if !matches!(inner, Type::Boolean | Type::Any) {
                return GateDecision::Invalid(vec![diagnostic(
                    "after",
                    format!("{reference} is {ty}, not a boolean"),
                )]);
            }
        }
        _ => {}
    }
    match gate {
        Gate::Step { id, accept_skip } => {
            let status = state.status(id);
            match status {
                StepStatus::Succeeded => GateDecision::Ready,
                StepStatus::Skipped if *accept_skip => GateDecision::Ready,
                StepStatus::Skipped => {
                    GateDecision::Skip(vec![SkipReason::Step { step: id.clone() }])
                }
                _ => GateDecision::Wait(vec![format!(
                    "after {} ({})",
                    gate.entry(),
                    status_name(&status)
                )]),
            }
        }
        Gate::Bool { reference, negate } => match resolve_reference(plan, state, reference) {
            BoundValue::Waiting => {
                let reason = match reference.parts() {
                    Ok(Reference { step: Some(id), .. }) => format!(
                        "after {} ({})",
                        gate.entry(),
                        status_name(&state.status(&id))
                    ),
                    _ => format!(
                        "after {} ({})",
                        gate.entry(),
                        wait_reference(reference, state)
                    ),
                };
                GateDecision::Wait(vec![reason])
            }
            BoundValue::Skipped(reason) => GateDecision::Skip(vec![reason]),
            BoundValue::Ready(value) => match value.as_value() {
                Value::Bool(value) if *value != *negate => GateDecision::Ready,
                Value::Bool(value) => GateDecision::Skip(vec![SkipReason::Boolean {
                    reference: reference.0.clone(),
                    value: Some(*value),
                    negate: *negate,
                }]),
                Value::Null => GateDecision::Skip(vec![SkipReason::Boolean {
                    reference: reference.0.clone(),
                    value: None,
                    negate: *negate,
                }]),
                value => GateDecision::Invalid(vec![diagnostic(
                    "after",
                    format!("{reference} is {value}, not a boolean"),
                )]),
            },
        },
        Gate::Unit { name, accept_skip } => {
            let Some(unit) = plan.units().get(name) else {
                return GateDecision::Invalid(vec![diagnostic("after", format!("no unit {name}"))]);
            };
            GateDecision::combine(unit.exits.iter().map(|id| {
                let status = state.status(id);
                match status {
                    StepStatus::Succeeded => GateDecision::Ready,
                    StepStatus::Skipped if *accept_skip => GateDecision::Ready,
                    StepStatus::Skipped => GateDecision::Skip(vec![SkipReason::Unit {
                        unit: name.clone(),
                        exit: id.clone(),
                    }]),
                    _ => GateDecision::Wait(vec![format!(
                        "after {} (exit {id} {})",
                        gate.entry(),
                        status_name(&status)
                    )]),
                }
            }))
        }
    }
}
/// Apply holds before inspecting gates or handoffs. Running/terminal work is
/// outside admission; reconcile only calls this for pending and skipped steps.
pub fn evaluate_step(plan: &Plan, state: &StateSnapshot, step: &Step) -> GateDecision {
    let holds: Vec<_> = [state.paused.waiting_reason(), step.paused.waiting_reason()]
        .into_iter()
        .flatten()
        .collect();
    if !holds.is_empty() {
        return GateDecision::Wait(holds);
    }
    evaluate_inputs(plan, state, step)
}
/// Why a pending step has not started, resources aside: its own pause, its project's,
/// then every handoff and gate entry still waiting, in order.
pub fn wait_reasons(plan: &Plan, state: &StateSnapshot, step: &Step) -> Vec<String> {
    let mut reasons: Vec<String> = step.paused.waiting_reason().into_iter().collect();
    if state.paused.is_paused() {
        reasons.push("project paused".into());
    }
    if let GateDecision::Wait(waiting) = evaluate_inputs(plan, state, step) {
        reasons.extend(waiting);
    }
    reasons
}
/// Handoffs and gates only, with no holds applied.
fn evaluate_inputs(plan: &Plan, state: &StateSnapshot, step: &Step) -> GateDecision {
    let handoffs =
        step.bindings.values().flat_map(Binding::references).map(
            |reference| match resolve_reference(plan, state, reference) {
                BoundValue::Ready(_) => GateDecision::Ready,
                BoundValue::Skipped(reason) => GateDecision::Skip(vec![reason]),
                BoundValue::Waiting => GateDecision::Wait(vec![wait_reference(reference, state)]),
            },
        );
    GateDecision::combine(
        handoffs.chain(
            step.after
                .iter()
                .map(|gate| evaluate_gate(plan, state, gate)),
        ),
    )
}

/// Pending/skipped admission decisions, including the external-work wait text.
pub fn readiness(plan: &Plan, state: &StateSnapshot) -> IndexMap<StepId, GateDecision> {
    plan.steps()
        .iter()
        .filter(|(id, _)| matches!(state.status(id), StepStatus::Pending | StepStatus::Skipped))
        .map(|(id, step)| {
            let decision = match evaluate_step(plan, state, step) {
                GateDecision::Ready if step.is_external() => GateDecision::Wait(vec![
                    "external: set its outputs with step_set_output".into(),
                ]),
                decision => decision,
            };
            (id.clone(), decision)
        })
        .collect()
}

/// Pure tick projection. Data staleness and skip propagation are computed in
/// dependency order. Old outputs/hashes stay visible during retry and staleness.
pub fn reconcile(plan: &Plan, state: &StateSnapshot) -> StateSnapshot {
    let mut next = state.clone();
    next.steps.retain(|id, _| plan.steps().contains_key(id));
    next.inputs
        .0
        .retain(|name, _| plan.inputs().contains_key(name));
    for id in plan.topological_order() {
        let step = &plan.steps()[id];
        let status = next.status(id);
        if matches!(status, StepStatus::Succeeded | StepStatus::Stale) {
            let upstream_stale = step
                .data_dependencies()
                .iter()
                .any(|id| next.status(id) == StepStatus::Stale);
            let current_hash = inputs_hash(plan, &next, step);
            let entry = next.steps.entry(id.clone()).or_default();
            if entry.status == StepStatus::Succeeded
                && (upstream_stale
                    || current_hash.is_some_and(|hash| Some(hash) != entry.inputs_hash))
            {
                entry.status = StepStatus::Stale;
            } else if entry.status == StepStatus::Stale
                && !upstream_stale
                && current_hash.is_some()
                && current_hash == entry.inputs_hash
            {
                entry.status = StepStatus::Succeeded;
            }
        }
        let status = next.status(id);
        if !matches!(status, StepStatus::Pending | StepStatus::Skipped)
            || step.paused.is_paused()
            || next.paused.is_paused()
        {
            continue;
        }
        let decision = evaluate_step(plan, &next, step);
        let entry = next.steps.entry(id.clone()).or_default();
        entry.queued.clear();
        match decision {
            GateDecision::Skip(reasons) => {
                entry.status = StepStatus::Skipped;
                entry.skipped = reasons;
                entry.error = None;
            }
            GateDecision::Invalid(errors) => {
                entry.status = StepStatus::Failed;
                entry.skipped.clear();
                entry.error = Some(
                    errors
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; "),
                );
            }
            GateDecision::Ready | GateDecision::Wait(_) => {
                entry.status = StepStatus::Pending;
                entry.skipped.clear();
                entry.error = None;
            }
        }
    }
    // A hold removes old queue annotations without reevaluating the skip reason.
    for (id, entry) in &mut next.steps {
        if entry.status == StepStatus::Pending
            && (next.paused.is_paused() || plan.steps()[id].paused.is_paused())
        {
            entry.queued.clear();
        }
    }
    next
}

/// Cached capacities and already granted section leases; running step needs are
/// added by the simulator. None means that a capacity fn has no cached value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CachedResources {
    pub capacities: IndexMap<String, Option<u64>>,
    pub leased: IndexMap<String, u64>,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DryRun {
    /// Every ready executable candidate, ignoring resource shortages.
    pub would_start: Vec<StepId>,
    /// Subset of would_start that admission would queue.
    pub would_queue: IndexMap<StepId, Vec<String>>,
    pub would_skip: IndexMap<StepId, Vec<SkipReason>>,
    pub would_stale: Vec<StepId>,
    pub errors: Vec<PathError>,
}
/// Simulate a validated candidate edit using no execution or capacity callbacks.
/// before_state/after_state allow plan-input and project-pause edits as well.
pub fn simulate_edit(
    before_plan: &Plan,
    before_state: &StateSnapshot,
    after_plan: &Plan,
    after_state: &StateSnapshot,
    resources: &CachedResources,
) -> DryRun {
    let before = reconcile(before_plan, before_state);
    let after = reconcile(after_plan, after_state);
    let mut result = DryRun::default();
    let mut held = resources.leased.clone();
    for (id, step) in after_plan.steps() {
        if after.status(id) == StepStatus::Running {
            for (name, need) in &step.needs {
                let amount = held.entry(name.clone()).or_default();
                *amount = amount.saturating_add(*need);
            }
        }
        let entry = after.steps.get(id);
        if after.status(id) == StepStatus::Stale && before.status(id) != StepStatus::Stale {
            result.would_stale.push(id.clone());
        }
        if let Some(entry) = entry {
            if entry.status == StepStatus::Skipped
                && before.steps.get(id).is_none_or(|old| {
                    old.status != StepStatus::Skipped || old.skipped != entry.skipped
                })
            {
                result.would_skip.insert(id.clone(), entry.skipped.clone());
            }
            if entry.status == StepStatus::Failed
                && before
                    .steps
                    .get(id)
                    .is_none_or(|old| old.status != StepStatus::Failed || old.error != entry.error)
                && let Some(error) = &entry.error
            {
                result
                    .errors
                    .push(diagnostic(&format!("steps.{id}"), error));
            }
        }
        if after.status(id) == StepStatus::Pending
            && !step.is_external()
            && evaluate_step(after_plan, &after, step) == GateDecision::Ready
        {
            result.would_start.push(id.clone());
        }
    }
    let mut admission: Vec<_> = after_plan
        .topological_order()
        .iter()
        .filter(|id| result.would_start.contains(id))
        .cloned()
        .collect();
    admission.sort_by_key(|id| std::cmp::Reverse(after_plan.steps()[id].priority));
    for id in admission {
        let step = &after_plan.steps()[&id];
        let short: Vec<_> = step
            .needs
            .iter()
            .filter(|(name, need)| {
                **need > 0
                    && resources
                        .capacities
                        .get(*name)
                        .copied()
                        .flatten()
                        .is_none_or(|capacity| {
                            capacity.saturating_sub(held.get(*name).copied().unwrap_or(0)) < **need
                        })
            })
            .map(|(name, _)| name.clone())
            .collect();
        if short.is_empty() {
            for (name, need) in &step.needs {
                let amount = held.entry(name.clone()).or_default();
                *amount = amount.saturating_add(*need);
            }
        } else {
            result.would_queue.insert(id, short);
        }
    }
    result
}
