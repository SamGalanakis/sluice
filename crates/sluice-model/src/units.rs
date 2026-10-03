//! Unit derivation, settlement, retry and reference-closed pruning.

use crate::{
    commands::StepStatus,
    gates::{Gate, GateDecision, Reference, StateSnapshot, evaluate_step},
    ids::{StepId, UnitName},
    plan::{Plan, diagnostic},
    types::PathError,
};
use indexmap::{IndexMap, IndexSet};
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub name: UnitName,
    pub tagged: bool,
    pub steps: Vec<StepId>,
    pub entries: Vec<StepId>,
    pub exits: Vec<StepId>,
}
/// Direct internal edges determine entries/sinks. Valid unit gates always cross
/// units, so expanding them cannot change internal entries or exits.
pub(crate) fn derive_units(plan: &Plan) -> IndexMap<UnitName, Unit> {
    let mut units = IndexMap::<UnitName, Unit>::new();
    for (id, step) in plan.steps() {
        let name = step.unit_name();
        units
            .entry(name.clone())
            .or_insert_with(|| Unit {
                name,
                tagged: step.tags.iter().any(|tag| tag.starts_with("unit:")),
                steps: vec![],
                entries: vec![],
                exits: vec![],
            })
            .steps
            .push(id.clone());
    }
    for unit in units.values_mut() {
        let mut used = IndexSet::new();
        for id in &unit.steps {
            let step = &plan.steps()[id];
            let mut dependencies = step.data_dependencies();
            for gate in &step.after {
                match gate {
                    Gate::Step { id, .. } => dependencies.push(id.clone()),
                    Gate::Bool { reference, .. } => {
                        if let Ok(Reference { step: Some(id), .. }) = reference.parts() {
                            dependencies.push(id);
                        }
                    }
                    Gate::Unit { name, .. } => {
                        if let Some(other) = plan.units().get(name) {
                            dependencies.extend(other.exits.iter().cloned());
                        }
                    }
                }
            }
            let internal: Vec<_> = dependencies
                .into_iter()
                .filter(|id| unit.steps.contains(id))
                .collect();
            if internal.is_empty() {
                unit.entries.push(id.clone());
            }
            used.extend(internal);
        }
        unit.exits = unit
            .steps
            .iter()
            .filter(|id| plan.steps()[*id].tags.iter().any(|tag| tag == "exit"))
            .cloned()
            .collect();
        if unit.exits.is_empty() {
            unit.exits = unit
                .steps
                .iter()
                .filter(|id| !used.contains(*id))
                .cloned()
                .collect();
        }
    }
    units
}
impl Unit {
    pub fn exit_success(&self, state: &StateSnapshot) -> bool {
        !self.exits.is_empty()
            && self
                .exits
                .iter()
                .all(|id| state.status(id) == StepStatus::Succeeded)
    }
    pub fn done(&self, state: &StateSnapshot) -> bool {
        self.steps.iter().all(|id| {
            matches!(
                state.status(id),
                StepStatus::Succeeded | StepStatus::Skipped
            )
        })
    }
    /// Resources are ignored: queued pending work is still startable. A ready
    /// core.external step is externally blocked and can settle.
    pub fn settled(&self, plan: &Plan, state: &StateSnapshot) -> bool {
        self.steps.iter().all(|id| match state.status(id) {
            StepStatus::Running => false,
            StepStatus::Pending => {
                let step = &plan.steps()[id];
                step.is_external() || evaluate_step(plan, state, step) != GateDecision::Ready
            }
            _ => true,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetryWalk {
    /// Explicit retry targets, in plan order.
    pub steps: Vec<StepId>,
    /// Failed/stale dependents to reset, in plan order.
    pub rearmed: Vec<StepId>,
    pub stopped_at: Vec<StepId>,
}
impl RetryWalk {
    pub fn resets(&self) -> impl Iterator<Item = &StepId> {
        self.steps.iter().chain(&self.rearmed)
    }
    /// Apply only the status reset. Retain visible outputs, data hashes and the
    /// project hold; step holds are immutable fields of the unchanged plan.
    pub fn apply(&self, state: &StateSnapshot) -> StateSnapshot {
        let mut next = state.clone();
        for id in self.resets() {
            let entry = next.steps.entry(id.clone()).or_default();
            entry.status = StepStatus::Pending;
            entry.error = None;
            entry.skipped.clear();
            entry.queued.clear();
        }
        next
    }
}
pub fn retry_walk(
    plan: &Plan,
    state: &StateSnapshot,
    selected: &[StepId],
) -> Result<RetryWalk, Vec<PathError>> {
    let mut errors = vec![];
    if selected.is_empty() {
        errors.push(diagnostic("steps", "select steps to retry"));
    }
    for id in selected {
        if !plan.steps().contains_key(id) {
            errors.push(diagnostic(&format!("steps.{id}"), "no such step"));
        } else if !matches!(
            state.status(id),
            StepStatus::Succeeded | StepStatus::Failed | StepStatus::Stale
        ) {
            errors.push(diagnostic(
                &format!("steps.{id}"),
                "retry requires succeeded, failed or stale",
            ));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let selected: IndexSet<_> = selected.iter().cloned().collect();
    let mut dependents = IndexMap::<StepId, Vec<StepId>>::new();
    for id in plan.steps().keys() {
        for dependency in plan.dependencies(id) {
            dependents
                .entry(dependency.clone())
                .or_default()
                .push(id.clone());
        }
    }
    let mut queue: VecDeque<_> = selected.iter().cloned().collect();
    let mut seen = selected.clone();
    let mut rearmed = IndexSet::new();
    let mut stopped = IndexSet::new();
    while let Some(id) = queue.pop_front() {
        for dependent in dependents.get(&id).into_iter().flatten() {
            if !seen.insert(dependent.clone()) {
                continue;
            }
            match state.status(dependent) {
                StepStatus::Failed | StepStatus::Stale => {
                    rearmed.insert(dependent.clone());
                    queue.push_back(dependent.clone());
                }
                StepStatus::Pending => queue.push_back(dependent.clone()),
                _ => {
                    stopped.insert(dependent.clone());
                }
            }
        }
    }
    let in_order = |ids: &IndexSet<StepId>| {
        plan.steps()
            .keys()
            .filter(|id| ids.contains(*id))
            .cloned()
            .collect()
    };
    Ok(RetryWalk {
        steps: in_order(&selected),
        rearmed: in_order(&rearmed),
        stopped_at: in_order(&stopped),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PruneHolder {
    Step(StepId),
    PlanOutput(String),
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneSet {
    pub units: Vec<UnitName>,
    pub steps: Vec<StepId>,
    pub kept: IndexMap<UnitName, PruneHolder>,
}
fn referenced_units(plan: &Plan, id: &StepId) -> IndexSet<UnitName> {
    let step = &plan.steps()[id];
    let mut units: IndexSet<_> = plan
        .dependencies(id)
        .iter()
        .filter_map(|id| plan.steps().get(id).map(|step| step.unit_name()))
        .collect();
    for gate in &step.after {
        if let Gate::Unit { name, .. } = gate {
            units.insert(name.clone());
        }
    }
    units
}
/// Greatest closed subset of the selected done units. Candidate consumers can
/// retain each other only after an outside reference retains one of them.
pub fn prune_closed(
    plan: &Plan,
    state: &StateSnapshot,
    selected: &[UnitName],
) -> Result<PruneSet, Vec<PathError>> {
    let mut errors = vec![];
    for name in selected {
        match plan.units().get(name) {
            None => errors.push(diagnostic(&format!("units.{name}"), "no such unit")),
            Some(unit) if !unit.done(state) => errors.push(diagnostic(
                &format!("units.{name}"),
                "only done units can be pruned",
            )),
            _ => {}
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut candidates: IndexSet<_> = selected.iter().cloned().collect();
    let mut kept = IndexMap::new();
    loop {
        let mut remove = IndexMap::new();
        for (name, reference) in plan.outputs() {
            if let Ok(Reference { step: Some(id), .. }) = reference.parts() {
                let unit = plan.steps()[&id].unit_name();
                if candidates.contains(&unit) {
                    remove
                        .entry(unit)
                        .or_insert_with(|| PruneHolder::PlanOutput(name.clone()));
                }
            }
        }
        for (id, step) in plan.steps() {
            if candidates.contains(&step.unit_name()) {
                continue;
            }
            for unit in referenced_units(plan, id) {
                if candidates.contains(&unit) {
                    remove
                        .entry(unit)
                        .or_insert_with(|| PruneHolder::Step(id.clone()));
                }
            }
        }
        if remove.is_empty() {
            break;
        }
        for (name, holder) in remove {
            candidates.shift_remove(&name);
            kept.insert(name, holder);
        }
    }
    Ok(PruneSet {
        units: plan
            .units()
            .keys()
            .filter(|name| candidates.contains(*name))
            .cloned()
            .collect(),
        steps: plan
            .steps()
            .iter()
            .filter(|(_, step)| candidates.contains(&step.unit_name()))
            .map(|(id, _)| id.clone())
            .collect(),
        kept: plan
            .units()
            .keys()
            .filter_map(|name| kept.shift_remove(name).map(|holder| (name.clone(), holder)))
            .collect(),
    })
}
