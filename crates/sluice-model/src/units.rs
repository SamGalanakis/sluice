//! Unit derivation, settlement, retry and reference-closed pruning.

use crate::{
    commands::StepStatus,
    gates::{Gate, GateDecision, Reference, StateSnapshot, evaluate_step},
    ids::{StepId, UnitName},
    plan::{Plan, diagnostic},
    types::PathError,
};
use indexmap::{IndexMap, IndexSet};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub name: UnitName,
    pub tagged: bool,
    pub steps: Vec<StepId>,
    pub entries: Vec<StepId>,
    pub exits: Vec<StepId>,
}
/// A unit's entry and exit steps from its members (in plan order): an entry reads or gates on
/// no other member; the exits are the members tagged `exit`, else those no other member
/// depends on. Unit gates always cross units (a gate on its own unit is refused), so they
/// never make an internal edge and are left out.
pub(crate) fn derive_unit(plan: &Plan, name: UnitName, members: Vec<StepId>) -> Unit {
    let tagged = plan.steps()[&members[0]]
        .tags
        .iter()
        .any(|tag| tag.starts_with("unit:"));
    let mut entries = vec![];
    let mut used = IndexSet::new();
    for id in &members {
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
                Gate::Unit { .. } => {}
            }
        }
        let internal: Vec<_> = dependencies
            .into_iter()
            .filter(|id| members.contains(id))
            .collect();
        if internal.is_empty() {
            entries.push(id.clone());
        }
        used.extend(internal);
    }
    let mut exits: Vec<StepId> = members
        .iter()
        .filter(|id| plan.steps()[*id].tags.iter().any(|tag| tag == "exit"))
        .cloned()
        .collect();
    if exits.is_empty() {
        exits = members
            .iter()
            .filter(|id| !used.contains(*id))
            .cloned()
            .collect();
    }
    Unit {
        name,
        tagged,
        steps: members,
        entries,
        exits,
    }
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

/// The plan's settled steps (§7.1): those that need an edit, a retry, an input or an output
/// set by hand before anything more happens to them. Succeeded, failed, stale and skipped
/// steps are settled; a pending step is settled when it is held: paused (or its project is),
/// a ready `core.external` step, or waiting while nothing it depends on can still move (each
/// dependency it waits on is itself settled). Running steps, pending steps about to start or
/// queued on resources, and pending steps waiting on running or startable work are not.
pub fn settled_steps(plan: &Plan, state: &StateSnapshot) -> IndexSet<StepId> {
    let mut settled = IndexSet::new();
    for id in plan.topological_order() {
        let held = match state.status(id) {
            StepStatus::Running => false,
            StepStatus::Pending => {
                let step = &plan.steps()[id];
                match evaluate_step(plan, state, step) {
                    GateDecision::Ready => step.is_external(),
                    GateDecision::Wait(_) => {
                        step.paused.is_paused()
                            || state.paused.is_paused()
                            || plan.dependencies(id).iter().all(|dependency| {
                                matches!(
                                    state.status(dependency),
                                    StepStatus::Succeeded | StepStatus::Skipped
                                ) || settled.contains(dependency)
                            })
                    }
                    // About to be skipped or failed by the next reconcile.
                    GateDecision::Skip(_) | GateDecision::Invalid(_) => false,
                }
            }
            _ => true,
        };
        if held {
            settled.insert(id.clone());
        }
    }
    settled
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PruneHolder {
    Step(StepId),
    PlanOutput(String),
    /// A keep pattern (`plan_prune`'s `keep`, a project's `prune_keep`) the unit's name matches.
    Keep(String),
}

/// The most keep patterns one prune or one project takes, and the longest pattern.
pub const KEEP_PATTERNS_MAX: usize = 64;
pub const KEEP_PATTERN_LEN_MAX: usize = 128;

/// Check keep patterns: at most 64, each 1 to 128 characters with no whitespace or control
/// character. `*` matches any run of characters (none included), `?` exactly one; every
/// other character matches itself.
pub fn check_keep(field: &str, patterns: &[String]) -> Result<(), Vec<PathError>> {
    let mut errors = vec![];
    if patterns.len() > KEEP_PATTERNS_MAX {
        errors.push(diagnostic(
            field,
            format!("at most {KEEP_PATTERNS_MAX} patterns"),
        ));
    }
    for (index, pattern) in patterns.iter().enumerate() {
        let length = pattern.chars().count();
        if length == 0 || length > KEEP_PATTERN_LEN_MAX {
            errors.push(diagnostic(
                &format!("{field}[{index}]"),
                format!("a pattern is 1 to {KEEP_PATTERN_LEN_MAX} characters"),
            ));
        } else if pattern.chars().any(|c| c.is_whitespace() || c.is_control()) {
            errors.push(diagnostic(
                &format!("{field}[{index}]"),
                "a pattern has no whitespace or control characters",
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// The first of `patterns` that `name` matches, whole (`*` any run, `?` one character).
pub fn keep_match<'a>(patterns: &'a [String], name: &str) -> Option<&'a str> {
    let name: Vec<char> = name.chars().collect();
    patterns
        .iter()
        .find(|pattern| glob(&pattern.chars().collect::<Vec<_>>(), &name))
        .map(String::as_str)
}

/// Iterative wildcard match with one backtrack point: linear in practice, never exponential.
fn glob(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
            }
            Some('?') => {
                p += 1;
                n += 1;
            }
            Some(c) if *c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((sp, sn)) => {
                    p = sp + 1;
                    n = sn + 1;
                    star = Some((sp, sn + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PruneSet {
    pub units: Vec<UnitName>,
    pub steps: Vec<StepId>,
    #[schemars(with = "std::collections::BTreeMap<UnitName, PruneHolder>")]
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
///
/// A selected unit whose name matches one of `keep` is never a candidate: it is kept with
/// the pattern, and its steps hold what they reference like any surviving step's.
pub fn prune_closed(
    plan: &Plan,
    state: &StateSnapshot,
    selected: &[UnitName],
    keep: &[String],
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
    let mut kept = IndexMap::new();
    let mut candidates = IndexSet::new();
    for name in selected {
        match keep_match(keep, name.as_str()) {
            Some(pattern) => {
                kept.insert(name.clone(), PruneHolder::Keep(pattern.to_owned()));
            }
            None => {
                candidates.insert(name.clone());
            }
        }
    }
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
