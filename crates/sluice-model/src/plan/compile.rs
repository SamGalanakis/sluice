//! Compiling rows: the whole plan (`compile_rows`) or what an edit changes on a certified base
//! (`compile_delta`). Both are one engine: a whole compile is the delta that puts every row
//! into an empty plan, so the two cannot disagree.
//!
//! The engine re-checks what each change can affect (`docs/design/plan-rows.md` §6.1): a put
//! step whole; a put or removed name's readers (bindings, gates and plan outputs, found through
//! the base's reverse index); an input put against the step of the same name; the units the
//! change touches, their entries and exits, and the steps gating on a unit whose exits changed;
//! cycles from the dependencies the edit adds. Its errors are today's whole-plan compiler's,
//! in the same order: each error is filed under the phase and the position of the row that
//! whole compile would have reported it at, and sorted.

use super::{
    Consumer, Plan, SignatureProvider, Step, check_bindings, check_output, compile_gates,
    diagnostic, expanded_dependencies, index::referenced_names, parse_input, parse_step,
};
use crate::{
    gates::Gate,
    ids::{StepId, UnitName},
    persistent::Tree,
    plan_rows::{PlanRows, RootSection},
    rpc::{JsonMap, JsonValue},
    types::PathError,
    units::derive_unit,
};
use indexmap::{IndexMap, IndexSet};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering as Atomic},
};

static FULL_COMPILES: AtomicU64 = AtomicU64::new(0);

/// How many whole compiles (`compile_rows`) this process has run since the last reset: lane
/// H's `full_compiles` counter (§11). Process-wide.
pub fn full_compiles() -> u64 {
    FULL_COMPILES.load(Atomic::SeqCst)
}
/// Reset the model's cost counters (`full_compiles`).
pub fn reset_counters() {
    FULL_COMPILES.store(0, Atomic::SeqCst);
}

/// One row's net change.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Change<T> {
    Removed,
    /// The same data at another position.
    Moved(u64),
    /// New data (or a new row) at this position.
    Put(u64, T),
}
/// An edit's net change to a plan's rows: what `compile_delta` compiles.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Delta {
    pub root_order: Vec<RootSection>,
    pub inputs: IndexMap<String, Change<JsonValue>>,
    pub outputs: IndexMap<String, Change<JsonMap>>,
    pub steps: IndexMap<StepId, Change<JsonMap>>,
}

/// What a compile touched beyond the rows it was given: what preparation reconciles, indexes
/// and checks from.
#[derive(Debug, Clone, Default)]
pub(crate) struct Touched {
    /// Steps put with new data (or added), whether or not they compiled.
    pub written: IndexSet<StepId>,
    pub moved: IndexSet<StepId>,
    pub removed: IndexSet<StepId>,
    /// Steps whose bindings and gates were compiled again: the written ones and the readers of
    /// every name the edit changed.
    pub recompiled: IndexSet<StepId>,
    /// Steps whose dependencies were worked out again.
    pub dependencies: IndexSet<StepId>,
    /// Units whose exit steps changed, appeared or went.
    pub exits: IndexSet<UnitName>,
    /// Plan inputs put with new data or removed.
    pub inputs: IndexSet<String>,
}

/// File each error under the phase and row whole compile reports it at.
#[derive(Default)]
struct Found(Vec<(u8, u64, PathError)>);
impl Found {
    fn add(&mut self, phase: u8, position: u64, errors: Vec<PathError>) {
        self.0
            .extend(errors.into_iter().map(|error| (phase, position, error)));
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    fn sorted(mut self) -> Vec<PathError> {
        self.0
            .sort_by_key(|(phase, position, _)| (*phase, *position));
        self.0.into_iter().map(|(_, _, error)| error).collect()
    }
}
// Whole compile's phases, in its order.
const INPUTS: u8 = 2;
const STEPS: u8 = 3;
const OUTPUTS: u8 = 4;
const BINDINGS: u8 = 5;
const GATES: u8 = 6;
const SINGLETONS: u8 = 8;
const UNIT_GATES: u8 = 9;
const CYCLES: u8 = 10;

/// Compile a plan from its rows, whole: a cold cache, `verify`, a converted project. The
/// errors are the whole-plan compiler's, in its order. Counted (`full_compiles`).
pub fn compile_rows(
    rows: &PlanRows,
    signatures: &impl SignatureProvider,
) -> Result<Plan, Vec<PathError>> {
    FULL_COMPILES.fetch_add(1, Atomic::SeqCst);
    let present = |section| rows.header.root_order.contains(&section);
    let delta = Delta {
        root_order: rows.header.root_order.clone(),
        inputs: rows
            .inputs
            .iter()
            .filter(|_| present(RootSection::Inputs))
            .map(|row| {
                (
                    row.name.clone(),
                    Change::Put(row.position, row.declaration.clone()),
                )
            })
            .collect(),
        outputs: rows
            .outputs
            .iter()
            .filter(|_| present(RootSection::Outputs))
            .map(|row| {
                (
                    row.name.clone(),
                    Change::Put(row.position, row.binding.clone()),
                )
            })
            .collect(),
        steps: rows
            .steps
            .iter()
            .filter(|_| present(RootSection::Steps))
            .map(|row| {
                (
                    row.step.clone(),
                    Change::Put(row.position, row.declaration.clone()),
                )
            })
            .collect(),
    };
    compile_delta(&Plan::empty(), &delta, signatures).map(|compiled| compiled.plan)
}

pub(crate) struct Compiled {
    pub plan: Plan,
    pub touched: Touched,
}

/// Compile `base` with `delta` applied. `base` must be certified (it compiled with no error);
/// only what the delta can affect is checked again.
pub(crate) fn compile_delta(
    base: &Plan,
    delta: &Delta,
    signatures: &impl SignatureProvider,
) -> Result<Compiled, Vec<PathError>> {
    let mut plan = base.clone();
    plan.order = OnceLock::new();
    plan.root_order = delta.root_order.clone();
    let mut found = Found::default();
    let mut touched = Touched::default();
    // Every step id or input name whose meaning the edit changes: its readers are checked again.
    let mut changed: IndexSet<String> = IndexSet::new();
    if !plan.root_order.contains(&RootSection::Steps) {
        found.add(
            STEPS,
            0,
            vec![diagnostic("steps", "required, an object of id -> step")],
        );
    }

    // Inputs. Every changed row leaves its position first, so moved rows never collide.
    let mut moved_inputs = IndexMap::new();
    for (name, change) in &delta.inputs {
        if let Some((_, declaration)) = plan.inputs.remove(name)
            && matches!(change, Change::Moved(_))
        {
            moved_inputs.insert(name.clone(), declaration);
        }
    }
    for (name, change) in &delta.inputs {
        match change {
            Change::Removed => {
                plan.written_inputs.remove(name);
                touched.inputs.insert(name.clone());
                changed.insert(name.clone());
            }
            Change::Moved(position) => {
                if let Some(declaration) = moved_inputs.shift_remove(name) {
                    plan.inputs.insert(name.clone(), *position, declaration);
                }
            }
            Change::Put(position, written) => {
                let (declaration, errors) = parse_input(name, written);
                found.add(INPUTS, *position, errors);
                plan.written_inputs.insert(name.clone(), written.clone());
                if let Some(declaration) = declaration {
                    plan.inputs
                        .insert(name.clone(), *position, Arc::new(declaration));
                }
                touched.inputs.insert(name.clone());
                changed.insert(name.clone());
            }
        }
    }

    // Steps on their own.
    let mut moved_steps = IndexMap::new();
    for (id, change) in &delta.steps {
        if let Some((_, step)) = plan.steps.remove(id)
            && matches!(change, Change::Moved(_))
        {
            moved_steps.insert(id.clone(), step);
        }
    }
    for (id, change) in &delta.steps {
        match change {
            Change::Removed => {
                touched.removed.insert(id.clone());
                changed.insert(id.to_string());
            }
            Change::Moved(position) => {
                if let Some(step) = moved_steps.shift_remove(id) {
                    plan.steps.insert(id.clone(), *position, step);
                }
                touched.moved.insert(id.clone());
            }
            Change::Put(position, declaration) => {
                let mut errors = vec![];
                let path = format!("steps.{id}");
                if ["owner", "orchestrator"].contains(&id.as_str()) {
                    errors.push(diagnostic(&path, "reserved message address"));
                }
                if plan.inputs.contains_key(id.as_str()) {
                    errors.push(diagnostic(
                        &path,
                        "plan inputs and steps share one namespace",
                    ));
                }
                if let Some(step) = parse_step(id.clone(), declaration, signatures, &mut errors) {
                    plan.steps.insert(id.clone(), *position, Arc::new(step));
                }
                found.add(STEPS, *position, errors);
                touched.written.insert(id.clone());
                changed.insert(id.to_string());
            }
        }
    }
    // An input put beside an untouched step of the same name: reported at the step.
    for name in &touched.inputs {
        if plan.inputs.contains_key(name)
            && let Ok(id) = StepId::new(name)
            && !matches!(delta.steps.get(&id), Some(Change::Put(..)))
            && let Some(position) = plan.steps.position(&id)
        {
            found.add(
                STEPS,
                position,
                vec![diagnostic(
                    &format!("steps.{id}"),
                    "plan inputs and steps share one namespace",
                )],
            );
        }
    }

    // Plan outputs: the put ones, and every untouched one reading a changed name.
    let mut moved_outputs = IndexMap::new();
    for (name, change) in &delta.outputs {
        if let Some((_, source)) = plan.outputs.remove(name)
            && matches!(change, Change::Moved(_))
        {
            moved_outputs.insert(name.clone(), source);
        }
    }
    let mut check: IndexMap<String, (u64, JsonMap)> = IndexMap::new();
    for (name, change) in &delta.outputs {
        match change {
            Change::Removed => {
                plan.written_outputs.remove(name);
            }
            Change::Moved(position) => {
                if let Some(source) = moved_outputs.shift_remove(name) {
                    plan.outputs.insert(name.clone(), *position, source);
                }
            }
            Change::Put(position, binding) => {
                plan.written_outputs.insert(name.clone(), binding.clone());
                check.insert(name.clone(), (*position, binding.clone()));
            }
        }
    }
    for name in &changed {
        for consumer in base.readers(name) {
            if let Consumer::Output(output) = consumer
                && !check.contains_key(output)
                && !matches!(
                    delta.outputs.get(output),
                    Some(Change::Put(..) | Change::Removed)
                )
                && let Some((position, binding)) = plan.output_row(output)
            {
                check.insert(output.clone(), (position, binding.clone()));
            }
        }
    }
    let mut outputs = vec![];
    for (name, (position, binding)) in &check {
        let (source, errors) = check_output(&plan, name, binding);
        found.add(OUTPUTS, *position, errors);
        outputs.push((name.clone(), *position, source));
    }
    for (name, position, source) in outputs {
        match source {
            Some(source) => plan.outputs.insert(name, position, Arc::new(source)),
            None => {
                plan.outputs.remove(&name);
            }
        }
    }

    // Bindings and gates: the written steps, and the readers of every changed name.
    let mut recompile: IndexSet<StepId> = touched
        .written
        .iter()
        .filter(|id| plan.steps.contains_key(*id))
        .cloned()
        .collect();
    for name in &changed {
        for consumer in base.readers(name) {
            if let Consumer::Step(id) = consumer
                && plan.steps.contains_key(id)
            {
                recompile.insert(id.clone());
            }
        }
    }
    let mut compiled = vec![];
    for id in &recompile {
        let step = plan.steps.get_shared(id).expect("a step to compile");
        let position = plan.steps.position(id).expect("a step's position");
        let (extras, binding_errors) = check_bindings(&plan, step);
        let (gates, gate_errors) = compile_gates(&plan, step);
        found.add(BINDINGS, position, binding_errors);
        found.add(GATES, position, gate_errors);
        compiled.push((id.clone(), position, extras, gates));
    }
    for (id, position, extras, gates) in compiled {
        let mut step = (**plan.steps.get_shared(&id).expect("a compiled step")).clone();
        step.extra_inputs = extras;
        step.after = gates;
        plan.steps.insert(id, position, Arc::new(step));
    }
    touched.recompiled = recompile;

    // A malformed unit tag cannot name a unit: whole compile stops here.
    if touched.written.iter().any(|id| {
        plan.steps.get(id).is_some_and(|step| {
            step.tags.iter().any(|tag| {
                tag.strip_prefix("unit:")
                    .is_some_and(|name| UnitName::new(name).is_err())
            })
        })
    }) {
        return Err(found.sorted());
    }

    // The reverse indexes of every step and output whose declaration changed.
    for id in touched.written.iter().chain(&touched.removed) {
        reindex_step(&mut plan, id, base.steps.get(id));
    }
    for (name, change) in &delta.outputs {
        if !matches!(change, Change::Moved(_)) {
            reindex_output(&mut plan, name, base.written_outputs.get(name));
        }
    }

    // Units: every unit a changed, moved or recompiled step left or joined, derived again.
    let mut units: IndexSet<UnitName> = IndexSet::new();
    for id in touched
        .written
        .iter()
        .chain(&touched.moved)
        .chain(&touched.removed)
        .chain(&touched.recompiled)
    {
        if let Some(step) = base.steps.get(id) {
            units.insert(step.unit_name());
        }
        if let Some(step) = plan.steps.get(id) {
            units.insert(step.unit_name());
        }
    }
    let mut derived = vec![];
    for name in &units {
        let mut members: Vec<(u64, StepId)> = base
            .units
            .get(name)
            .into_iter()
            .flat_map(|unit| unit.steps.iter())
            .filter(|id| !delta.steps.contains_key(*id))
            .filter_map(|id| Some((plan.steps.position(id)?, id.clone())))
            .collect();
        for id in delta.steps.keys() {
            if let (Some(step), Some(position)) = (plan.steps.get(id), plan.steps.position(id))
                && step.unit_name() == *name
            {
                members.push((position, id.clone()));
            }
        }
        members.sort();
        let old_exits = base.units.get(name).map(|unit| &unit.exits);
        if members.is_empty() {
            if old_exits.is_some() {
                touched.exits.insert(name.clone());
            }
            continue;
        }
        let first = members[0].0;
        let unit = derive_unit(
            &plan,
            name.clone(),
            members.into_iter().map(|(_, id)| id).collect(),
        );
        if old_exits != Some(&unit.exits) {
            touched.exits.insert(name.clone());
        }
        derived.push((first, unit));
    }
    // Every re-derived unit leaves its position first: a unit sits at its first member's.
    for name in &units {
        plan.units.remove(name);
    }
    for (first, unit) in derived {
        plan.units.insert(unit.name.clone(), first, Arc::new(unit));
    }

    // A singleton unit's name may not be a tagged unit's.
    let mut candidates: IndexSet<StepId> = IndexSet::new();
    for id in &touched.written {
        if let Some(step) = plan.steps.get(id) {
            if !step.tags.iter().any(|tag| tag.starts_with("unit:")) {
                candidates.insert(id.clone());
            }
            for tag in &step.tags {
                if let Some(name) = tag.strip_prefix("unit:")
                    && let Ok(name) = StepId::new(name)
                {
                    candidates.insert(name);
                }
            }
        }
    }
    for id in &candidates {
        let Some(step) = plan.steps.get(id) else {
            continue;
        };
        if step.tags.iter().any(|tag| tag.starts_with("unit:")) {
            continue;
        }
        let tag = format!("unit:{id}");
        let tagged_member = UnitName::new(id.as_str())
            .ok()
            .and_then(|name| plan.units.get(&name))
            .is_some_and(|unit| {
                unit.steps
                    .iter()
                    .any(|member| plan.steps[member].tags.contains(&tag))
            });
        let written = touched
            .written
            .iter()
            .filter_map(|other| plan.steps.get(other))
            .any(|other| other.tags.contains(&tag));
        if tagged_member || written {
            found.add(
                SINGLETONS,
                plan.steps.position(id).expect("a step's position"),
                vec![diagnostic(
                    &format!("steps.{id}.tags"),
                    "singleton unit name collides with a tagged unit",
                )],
            );
        }
    }

    // Unit gates: the recompiled steps, and the readers of every unit that went.
    let mut unit_gates: IndexSet<StepId> = touched.recompiled.clone();
    for name in &touched.exits {
        if !plan.units.contains_key(name) {
            unit_gates.extend(plan.unit_readers(name).cloned());
        }
    }
    for id in &unit_gates {
        let Some(step) = plan.steps.get(id) else {
            continue;
        };
        let mut errors = vec![];
        for gate in &step.after {
            if let Gate::Unit { name, .. } = gate {
                if !plan.units.contains_key(name) {
                    errors.push(diagnostic(
                        &format!("steps.{id}.after"),
                        format!("no unit {name}"),
                    ));
                }
                if *name == step.unit_name() {
                    errors.push(diagnostic(
                        &format!("steps.{id}.after"),
                        "a unit cannot depend on its own exits",
                    ));
                }
            }
        }
        found.add(
            UNIT_GATES,
            plan.steps.position(id).expect("a step's position"),
            errors,
        );
    }

    // Dependencies: the recompiled steps, and the steps gating on a unit whose exits changed.
    for id in touched.written.iter().chain(&touched.removed) {
        if !plan.steps.contains_key(id) {
            plan.dependencies.remove(id);
        }
    }
    let mut again: IndexSet<StepId> = touched.recompiled.clone();
    for name in &touched.exits {
        again.extend(plan.unit_readers(name).cloned());
    }
    let mut added: Vec<(StepId, StepId)> = vec![];
    for id in &again {
        let Some(step) = plan.steps.get(id) else {
            continue;
        };
        let dependencies = expanded_dependencies(&plan, step);
        let old = base.dependencies(id);
        added.extend(
            dependencies
                .iter()
                .filter(|dependency| !old.contains(dependency))
                .map(|dependency| (id.clone(), dependency.clone())),
        );
        plan.dependencies.insert(id.clone(), dependencies.into());
    }
    touched.dependencies = again;
    // A cycle the edit makes runs through one of the dependencies it adds: search from each
    // added dependency for the step that added it. Whole compile names the first cycle its
    // depth-first walk finds, so the witness is that walk's.
    if added
        .iter()
        .any(|(id, dependency)| reaches(&plan, dependency, id))
        && let Err(cycle) = plan.cycle_free_order()
    {
        found.add(
            CYCLES,
            0,
            vec![diagnostic(
                &format!("steps.{}", cycle[0]),
                format!(
                    "dependency cycle {}",
                    cycle
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" -> ")
                ),
            )],
        );
    }

    if !found.is_empty() {
        return Err(found.sorted());
    }
    Ok(Compiled { plan, touched })
}

/// Whether `to` is reachable from `from` along dependencies.
fn reaches(plan: &Plan, from: &StepId, to: &StepId) -> bool {
    if !plan.steps.contains_key(from) {
        return false;
    }
    let mut seen: IndexSet<&StepId> = IndexSet::from([from]);
    let mut stack = vec![from];
    while let Some(id) = stack.pop() {
        if id == to {
            return true;
        }
        for dependency in plan.dependencies(id) {
            if let Some(step) = plan.steps.get(dependency)
                && seen.insert(&step.id)
            {
                stack.push(&step.id);
            }
        }
    }
    false
}

fn add_to<K: Ord + Clone, M: Ord + Clone>(tree: &mut Tree<K, Tree<M, ()>>, key: K, member: M) {
    let mut members = tree.get(&key).cloned().unwrap_or_default();
    members.insert(member, ());
    tree.insert(key, members);
}
fn remove_from<K: Ord + Clone, M: Ord + Clone>(
    tree: &mut Tree<K, Tree<M, ()>>,
    key: &K,
    member: &M,
) {
    if let Some(members) = tree.get(key) {
        let mut members = members.clone();
        members.remove(member);
        if members.is_empty() {
            tree.remove(key);
        } else {
            tree.insert(key.clone(), members);
        }
    }
}

/// Move a step's entries in the reverse indexes from its old declaration to its new one.
fn reindex_step(plan: &mut Plan, id: &StepId, old: Option<&Step>) {
    let consumer = Consumer::Step(id.clone());
    if let Some(old) = old {
        let (names, units) = referenced_names(&old.declaration);
        for name in names {
            remove_from(&mut plan.readers, &name, &consumer);
        }
        for unit in units {
            remove_from(&mut plan.unit_readers, &unit, id);
        }
        for resource in old.needs.keys() {
            remove_from(&mut plan.needing, resource, id);
        }
        for tag in &old.tags {
            remove_from(&mut plan.tagged, tag, id);
        }
    }
    let Some(new) = plan.steps.get_shared(id).cloned() else {
        return;
    };
    let (names, units) = referenced_names(&new.declaration);
    for name in names {
        add_to(&mut plan.readers, name, consumer.clone());
    }
    for unit in units {
        add_to(&mut plan.unit_readers, unit, id.clone());
    }
    for resource in new.needs.keys() {
        add_to(&mut plan.needing, resource.clone(), id.clone());
    }
    for tag in &new.tags {
        add_to(&mut plan.tagged, tag.clone(), id.clone());
    }
}
fn reindex_output(plan: &mut Plan, name: &str, old: Option<&JsonMap>) {
    let consumer = Consumer::Output(name.to_owned());
    let source = |binding: &JsonMap| {
        binding
            .0
            .get("source")
            .and_then(|source| source.as_value().as_str())
            .and_then(|text| crate::gates::ValueRef(text.to_owned()).parts().ok())
            .map(|parts| parts.step.map_or(parts.name, |step| step.to_string()))
    };
    if let Some(old) = old.and_then(source) {
        remove_from(&mut plan.readers, &old, &consumer);
    }
    if let Some(new) = plan
        .written_outputs
        .get(name)
        .cloned()
        .as_ref()
        .and_then(source)
    {
        add_to(&mut plan.readers, new, consumer);
    }
}
