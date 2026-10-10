//! `plan_edit`'s operations (`plan_rows::PlanOp`, `docs/design/plan-rows.md` §7.6) applied in
//! order to one candidate: an overlay of changed rows on a compiled base, never a copy of it.
//!
//! What a refused operation says (the contract fixes the paths `ops[<i>].<field>`):
//!
//! | Operation | Refusal |
//! |---|---|
//! | `input.remove` | `ops[i].name: no input <name>` |
//! | `output.remove` | `ops[i].name: no output <name>` |
//! | `step.add` | `ops[i].step: step <id> already exists` |
//! | `step.update` | `ops[i].step: no step <id>` |
//! | `step.remove` | `ops[i].steps[j]: no step <id>` |
//! | `edge.add`, `edge.remove` | `ops[i].step: no step <id>`, `ops[i].step: no unit <u>` (or the unit name's own error); `ops[i].after[j]: <entry error>`, `ops[i].after[j]: no unit <u>` |
//! | `unit.add` | `ops[i].recipe: no recipe <r>`; `ops[i].recipe: recipe <r> (<scope>): <error>`; `ops[i].params.unit: must match the requested unit`; the expansion's own errors under `ops[i].`; `ops[i].steps.<id>: already exists in the plan` |
//! | `unit.update` | `ops[i].unit: no unit <u>`; `ops[i].changes.<id>: not a step of unit <u>` |
//! | `unit.remove` | `ops[i].unit: no unit <u>` |
//! | `order.set` | `ops[i].ids[j]: no <step\|input\|output> <id>`; `ops[i].ids[j]: <id> is listed twice`; `ops[i].ids: <id> is missing` |
//!
//! An entry error is the entry's syntax and classification (`Gate::compile`'s messages) and a
//! named unit's existence; an entry's type is the candidate's validation. Every operation is
//! checked; a refused one changes nothing, and the ones after it see the candidate without it.
//!
//! Net effect: a row is put only when its position or its data changes (`hash::data_equal`, so
//! key order alone is no change); a row whose data the edit leaves equal keeps its stored
//! bytes. `start: false` appends `"paused": true` to every added declaration lacking `paused`.

use super::{
    Change, Delta, Plan, SignatureProvider, decl_dependencies, diagnostic, entry_error, unit_of,
};
use crate::{
    hash::{data_equal, data_equal_maps},
    ids::{StepId, UnitName},
    plan_rows::{OrderCollection, PlanChange, PlanOp, RootSection, StepChanges},
    recipe::{ExpansionOptions, RecipeEntry},
    rpc::{JsonMap, JsonValue},
    types::PathError,
};
use indexmap::{IndexMap, IndexSet};
use serde_json::Value;

/// The canonical order of the root sections (§2.4).
const CANONICAL: [RootSection; 3] = [
    RootSection::Inputs,
    RootSection::Outputs,
    RootSection::Steps,
];

/// What applying the operations made.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Applied {
    /// The net row changes, to compile.
    pub delta: Delta,
    /// The net row changes as the log records them (§5.1 order); empty when nothing changes.
    pub changes: Vec<PlanChange>,
    /// The ids the `step.add` and `unit.add` operations added, in operation order (a unit's in
    /// recipe order); `None` when there are none.
    pub added: Option<Vec<StepId>>,
}

/// One collection's changed rows over the base: key → the row now, or `None` once removed.
type Overlay<K, V> = IndexMap<K, Option<(u64, V)>>;

/// The candidate so far: the base, with the rows the operations changed laid over it.
struct Work<'a> {
    base: &'a Plan,
    root_order: Vec<RootSection>,
    inputs: Overlay<String, JsonValue>,
    outputs: Overlay<String, JsonMap>,
    steps: Overlay<StepId, JsonMap>,
}
impl<'a> Work<'a> {
    fn new(base: &'a Plan) -> Self {
        Self {
            base,
            root_order: base.root_order().to_vec(),
            inputs: IndexMap::new(),
            outputs: IndexMap::new(),
            steps: IndexMap::new(),
        }
    }
    fn input(&self, name: &str) -> Option<(u64, &JsonValue)> {
        match self.inputs.get(name) {
            Some(row) => row.as_ref().map(|(p, v)| (*p, v)),
            None => self.base.input_row(name),
        }
    }
    fn output(&self, name: &str) -> Option<(u64, &JsonMap)> {
        match self.outputs.get(name) {
            Some(row) => row.as_ref().map(|(p, v)| (*p, v)),
            None => self.base.output_row(name),
        }
    }
    fn step(&self, id: &StepId) -> Option<(u64, &JsonMap)> {
        match self.steps.get(id) {
            Some(row) => row.as_ref().map(|(p, v)| (*p, v)),
            None => Some((
                self.base.position(id)?,
                &self.base.steps().get(id)?.declaration,
            )),
        }
    }
    fn has_step(&self, name: &str) -> bool {
        StepId::new(name).is_ok_and(|id| self.step(&id).is_some())
    }
    /// A put into an absent section adds it before the first present section that follows it
    /// canonically, else at the end (§2.4).
    fn ensure(&mut self, section: RootSection) {
        if self.root_order.contains(&section) {
            return;
        }
        let rank = |s: &RootSection| CANONICAL.iter().position(|c| c == s).expect("section");
        let at = self
            .root_order
            .iter()
            .position(|s| rank(s) > rank(&section))
            .unwrap_or(self.root_order.len());
        self.root_order.insert(at, section);
    }
    /// One past the collection's largest position (0 when it is empty).
    fn next_step_position(&self) -> u64 {
        let base = self
            .base
            .steps()
            .iter_positions_rev()
            .find(|(_, id, _)| !self.steps.contains_key(*id))
            .map(|(position, _, _)| position);
        next(base, self.steps.values())
    }
    fn next_input_position(&self) -> u64 {
        let base = self
            .base
            .inputs()
            .iter_positions_rev()
            .find(|(_, name, _)| !self.inputs.contains_key(*name))
            .map(|(position, _, _)| position);
        next(base, self.inputs.values())
    }
    fn next_output_position(&self) -> u64 {
        let base = self
            .base
            .outputs()
            .iter_positions_rev()
            .find(|(_, name, _)| !self.outputs.contains_key(*name))
            .map(|(position, _, _)| position);
        next(base, self.outputs.values())
    }
    /// A unit's members in the candidate, in position order: a step's unit is its first
    /// `unit:` tag, else its id (`steps.unit`, §2.5). Empty when there is no such unit.
    fn members(&self, unit: &str) -> Vec<StepId> {
        let mut members: Vec<(u64, StepId)> = UnitName::new(unit)
            .ok()
            .and_then(|name| self.base.units().get(&name))
            .into_iter()
            .flat_map(|unit| &unit.steps)
            .filter(|id| !self.steps.contains_key(*id))
            .filter_map(|id| Some((self.base.position(id)?, id.clone())))
            .collect();
        for (id, row) in &self.steps {
            if let Some((position, declaration)) = row
                && unit_of(id.as_str(), declaration) == unit
            {
                members.push((*position, id.clone()));
            }
        }
        members.sort();
        members.into_iter().map(|(_, id)| id).collect()
    }
    /// A unit's entry steps (`units::derive_unit`, from declarations): members none of whose
    /// bindings or gates names another member.
    fn entries(&self, members: &[StepId]) -> Vec<StepId> {
        members
            .iter()
            .filter(|id| {
                let (_, declaration) = self.step(id).expect("a member");
                !decl_dependencies(declaration, &|name| self.has_step(name))
                    .iter()
                    .any(|d| members.iter().any(|m| m.as_str() == d))
            })
            .cloned()
            .collect()
    }
    /// Every current key of a collection, in position order (`order.set`: O(collection)).
    fn keys(&self, collection: OrderCollection) -> Vec<String> {
        let mut keys: Vec<(u64, String)> = match collection {
            OrderCollection::Steps => {
                let mut keys: Vec<_> = self
                    .base
                    .steps()
                    .iter_positions()
                    .filter(|(_, id, _)| !self.steps.contains_key(*id))
                    .map(|(p, id, _)| (p, id.to_string()))
                    .collect();
                keys.extend(
                    self.steps
                        .iter()
                        .filter_map(|(id, row)| Some((row.as_ref()?.0, id.to_string()))),
                );
                keys
            }
            OrderCollection::Inputs => {
                let mut keys: Vec<_> = self
                    .base
                    .inputs()
                    .iter_positions()
                    .filter(|(_, name, _)| !self.inputs.contains_key(*name))
                    .map(|(p, name, _)| (p, name.clone()))
                    .collect();
                keys.extend(
                    self.inputs
                        .iter()
                        .filter_map(|(name, row)| Some((row.as_ref()?.0, name.clone()))),
                );
                keys
            }
            OrderCollection::Outputs => {
                let mut keys: Vec<_> = self
                    .base
                    .outputs()
                    .iter_positions()
                    .filter(|(_, name, _)| !self.outputs.contains_key(*name))
                    .map(|(p, name, _)| (p, name.clone()))
                    .collect();
                keys.extend(
                    self.outputs
                        .iter()
                        .filter_map(|(name, row)| Some((row.as_ref()?.0, name.clone()))),
                );
                keys
            }
        };
        keys.sort();
        keys.into_iter().map(|(_, key)| key).collect()
    }
}
fn next<'v, V: 'v>(base: Option<u64>, overlay: impl Iterator<Item = &'v Option<(u64, V)>>) -> u64 {
    overlay
        .filter_map(|row| row.as_ref().map(|(p, _)| *p))
        .chain(base)
        .max()
        .map_or(0, |max| max + 1)
}

/// Apply `step.update`'s changes to a declaration: a given field replaces that key (an
/// existing key keeps its place, a new one is appended), null removes it.
pub(crate) fn update(declaration: &mut JsonMap, changes: &StepChanges) {
    for (key, value) in changes.fields() {
        match value {
            Some(value) => {
                declaration.0.insert(key.to_owned(), value.clone());
            }
            None => {
                declaration.0.shift_remove(key);
            }
        }
    }
}

/// Apply `ops` in order to one candidate built from `base`. Every refusal is collected
/// (`ops[i]…`); none means the candidate's net changes, to be compiled and validated.
pub(crate) fn apply_ops(
    base: &Plan,
    ops: &[PlanOp],
    start: bool,
    recipes: &IndexMap<String, RecipeEntry>,
    signatures: &impl SignatureProvider,
) -> Result<Applied, Vec<PathError>> {
    let mut work = Work::new(base);
    let mut errors = vec![];
    let mut added: Vec<StepId> = vec![];
    for (index, op) in ops.iter().enumerate() {
        let at = |field: &str| format!("ops[{index}].{field}");
        let mut refused =
            |field: &str, message: String| errors.push(diagnostic(&at(field), message));
        match op {
            PlanOp::InputPut { name, declaration } => {
                work.ensure(RootSection::Inputs);
                let position = match work.input(name) {
                    Some((position, _)) => position,
                    None => work.next_input_position(),
                };
                work.inputs
                    .insert(name.clone(), Some((position, declaration.clone())));
            }
            PlanOp::InputRemove { name } => {
                if work.input(name).is_some() {
                    work.inputs.insert(name.clone(), None);
                } else {
                    refused("name", format!("no input {name}"));
                }
            }
            PlanOp::OutputPut { name, source } => {
                work.ensure(RootSection::Outputs);
                let binding = JsonMap(IndexMap::from([(
                    "source".to_owned(),
                    JsonValue::try_from(Value::String(source.clone())).expect("a string"),
                )]));
                let position = match work.output(name) {
                    Some((position, _)) => position,
                    None => work.next_output_position(),
                };
                work.outputs.insert(name.clone(), Some((position, binding)));
            }
            PlanOp::OutputRemove { name } => {
                if work.output(name).is_some() {
                    work.outputs.insert(name.clone(), None);
                } else {
                    refused("name", format!("no output {name}"));
                }
            }
            PlanOp::StepAdd { step, spec } => {
                if work.step(step).is_some() {
                    refused("step", format!("step {step} already exists"));
                    continue;
                }
                work.ensure(RootSection::Steps);
                let position = work.next_step_position();
                work.steps
                    .insert(step.clone(), Some((position, spec.clone())));
                added.push(step.clone());
            }
            PlanOp::StepUpdate { step, changes } => match work.step(step) {
                Some((position, declaration)) => {
                    let mut declaration = declaration.clone();
                    update(&mut declaration, changes);
                    work.steps
                        .insert(step.clone(), Some((position, declaration)));
                }
                None => refused("step", format!("no step {step}")),
            },
            PlanOp::StepRemove { steps } => {
                let mut missing = false;
                for (j, id) in steps.iter().enumerate() {
                    if work.step(id).is_none() {
                        refused(&format!("steps[{j}]"), format!("no step {id}"));
                        missing = true;
                    }
                }
                if !missing {
                    for id in steps {
                        work.steps.insert(id.clone(), None);
                    }
                }
            }
            PlanOp::EdgeAdd { step, after } | PlanOp::EdgeRemove { step, after } => {
                let adding = matches!(op, PlanOp::EdgeAdd { .. });
                let mut refusals = vec![];
                let targets = match step.strip_prefix("unit:") {
                    Some(name) => match UnitName::new(name) {
                        Err(error) => {
                            refusals.push(("step".to_owned(), error.to_string()));
                            vec![]
                        }
                        Ok(_) => {
                            let members = work.members(name);
                            if members.is_empty() {
                                refusals.push(("step".to_owned(), format!("no unit {name}")));
                            }
                            work.entries(&members)
                        }
                    },
                    None if work.has_step(step) => vec![StepId::new(step).expect("a step id")],
                    None => {
                        refusals.push(("step".to_owned(), format!("no step {step}")));
                        vec![]
                    }
                };
                for (j, entry) in after.iter().enumerate() {
                    if let Some(error) = entry_error(entry, &|name| work.has_step(name), &|unit| {
                        !work.members(unit).is_empty()
                    }) {
                        refusals.push((format!("after[{j}]"), error));
                    }
                }
                if !refusals.is_empty() {
                    for (field, message) in refusals {
                        refused(&field, message);
                    }
                    continue;
                }
                for id in targets {
                    let (position, declaration) = work.step(&id).expect("a target");
                    let old: Vec<String> = declaration
                        .0
                        .get("after")
                        .and_then(|a| a.as_value().as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect();
                    let new: Vec<String> = if adding {
                        old.iter()
                            .cloned()
                            .chain(after.iter().cloned())
                            .collect::<IndexSet<_>>()
                            .into_iter()
                            .collect()
                    } else {
                        old.iter().filter(|e| !after.contains(e)).cloned().collect()
                    };
                    if new == old {
                        continue;
                    }
                    let mut declaration = declaration.clone();
                    if new.is_empty() {
                        declaration.0.shift_remove("after");
                    } else {
                        declaration.0.insert(
                            "after".into(),
                            JsonValue::try_from(serde_json::json!(new)).expect("strings"),
                        );
                    }
                    work.steps.insert(id, Some((position, declaration)));
                }
            }
            PlanOp::UnitAdd {
                recipe,
                unit,
                params,
                after,
                inputs,
                tags,
            } => match expand_unit(
                &work,
                recipes,
                signatures,
                recipe,
                unit,
                params,
                &ExpansionOptions {
                    start: true,
                    tags: tags.clone(),
                    after: after.clone(),
                    inputs: inputs.clone(),
                },
            ) {
                Err(found) => {
                    for e in found {
                        refused(&e.path, e.message);
                    }
                }
                Ok(steps) => {
                    work.ensure(RootSection::Steps);
                    for (id, declaration) in steps {
                        let position = work.next_step_position();
                        work.steps.insert(id.clone(), Some((position, declaration)));
                        added.push(id);
                    }
                }
            },
            PlanOp::UnitUpdate { unit, changes } => {
                let members = work.members(unit.as_str());
                if members.is_empty() {
                    refused("unit", format!("no unit {unit}"));
                    continue;
                }
                let strangers: Vec<_> = changes.keys().filter(|id| !members.contains(id)).collect();
                if !strangers.is_empty() {
                    for id in strangers {
                        refused(
                            &format!("changes.{id}"),
                            format!("not a step of unit {unit}"),
                        );
                    }
                    continue;
                }
                for (id, change) in changes {
                    let (position, declaration) = work.step(id).expect("a member");
                    let mut declaration = declaration.clone();
                    update(&mut declaration, change);
                    work.steps.insert(id.clone(), Some((position, declaration)));
                }
            }
            PlanOp::UnitRemove { unit } => {
                let members = work.members(unit.as_str());
                if members.is_empty() {
                    refused("unit", format!("no unit {unit}"));
                }
                for id in members {
                    work.steps.insert(id, None);
                }
            }
            PlanOp::OrderSet { collection, ids } => {
                let members = work.keys(*collection);
                let noun = match collection {
                    OrderCollection::Steps => "step",
                    OrderCollection::Inputs => "input",
                    OrderCollection::Outputs => "output",
                };
                let known: IndexSet<&str> = members.iter().map(String::as_str).collect();
                let mut seen = IndexSet::new();
                let mut bad = false;
                for (j, id) in ids.iter().enumerate() {
                    if !known.contains(id.as_str()) {
                        refused(&format!("ids[{j}]"), format!("no {noun} {id}"));
                        bad = true;
                    } else if !seen.insert(id.as_str()) {
                        refused(&format!("ids[{j}]"), format!("{id} is listed twice"));
                        bad = true;
                    }
                }
                for id in members.iter().filter(|id| !seen.contains(id.as_str())) {
                    refused("ids", format!("{id} is missing"));
                    bad = true;
                }
                if bad {
                    continue;
                }
                for (position, id) in ids.iter().enumerate() {
                    let position = position as u64;
                    match collection {
                        OrderCollection::Steps => {
                            let id = StepId::new(id).expect("a step id");
                            let (_, declaration) = work.step(&id).expect("a member");
                            let declaration = declaration.clone();
                            work.steps.insert(id, Some((position, declaration)));
                        }
                        OrderCollection::Inputs => {
                            let (_, declaration) = work.input(id).expect("a member");
                            let declaration = declaration.clone();
                            work.inputs
                                .insert(id.clone(), Some((position, declaration)));
                        }
                        OrderCollection::Outputs => {
                            let (_, binding) = work.output(id).expect("a member");
                            let binding = binding.clone();
                            work.outputs.insert(id.clone(), Some((position, binding)));
                        }
                    }
                }
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    if !start {
        for (id, row) in &mut work.steps {
            if let Some((_, declaration)) = row
                && base.steps().get(id).is_none()
                && !declaration.0.contains_key("paused")
            {
                declaration.0.insert(
                    "paused".into(),
                    JsonValue::try_from(Value::Bool(true)).expect("bool"),
                );
            }
        }
    }
    let delta = Delta {
        root_order: work.root_order.clone(),
        inputs: net(
            &work.inputs,
            |name| base.input_row(name),
            |a, b| data_equal(a.as_value(), b.as_value()).unwrap_or(false),
        ),
        outputs: net(
            &work.outputs,
            |name| base.output_row(name),
            |a, b| data_equal_maps(a, b).unwrap_or(false),
        ),
        steps: net(
            &work.steps,
            |id| Some((base.position(id)?, &base.steps().get(id)?.declaration)),
            |a, b| data_equal_maps(a, b).unwrap_or(false),
        ),
    };
    let changes = changes(base, &delta);
    Ok(Applied {
        delta,
        changes,
        added: (!added.is_empty()).then_some(added),
    })
}

/// Each overlaid row's net change against the base: removed, moved with the same data (it
/// keeps its stored bytes), or put with new data; nothing when it is as it was.
fn net<'b, K: Clone + std::hash::Hash + Eq, V: Clone + 'b>(
    overlay: &Overlay<K, V>,
    base: impl Fn(&K) -> Option<(u64, &'b V)>,
    same: impl Fn(&V, &V) -> bool,
) -> IndexMap<K, Change<V>> {
    let mut out = IndexMap::new();
    for (key, row) in overlay {
        let change = match (row, base(key)) {
            (None, None) => continue,
            (None, Some(_)) => Change::Removed,
            (Some((position, value)), None) => Change::Put(*position, value.clone()),
            (Some((position, value)), Some((old_position, old))) => {
                if !same(value, old) {
                    Change::Put(*position, value.clone())
                } else if *position != old_position {
                    Change::Moved(*position)
                } else {
                    continue;
                }
            }
        };
        out.insert(key.clone(), change);
    }
    out
}

/// The net changes as the log records them (§5.1): `header.put` when the root order differs,
/// then deletes (inputs, outputs, steps) each in key order, then puts (inputs, outputs, steps)
/// each in position order. A moved row's put carries its stored declaration.
fn changes(base: &Plan, delta: &Delta) -> Vec<PlanChange> {
    let mut out = vec![];
    if base.root_order() != delta.root_order.as_slice() {
        out.push(PlanChange::HeaderPut {
            root_order: delta.root_order.clone(),
        });
    }
    let mut deletes: Vec<String> = removed(&delta.inputs);
    deletes.sort();
    out.extend(
        deletes
            .into_iter()
            .map(|name| PlanChange::InputDelete { name }),
    );
    let mut deletes: Vec<String> = removed(&delta.outputs);
    deletes.sort();
    out.extend(
        deletes
            .into_iter()
            .map(|name| PlanChange::OutputDelete { name }),
    );
    let mut deletes: Vec<StepId> = removed(&delta.steps);
    deletes.sort();
    out.extend(
        deletes
            .into_iter()
            .map(|step| PlanChange::StepDelete { step }),
    );
    let mut puts: Vec<_> = delta
        .inputs
        .iter()
        .filter_map(|(name, change)| {
            Some(match change {
                Change::Removed => return None,
                Change::Moved(position) => (*position, name, base.input_row(name)?.1.clone()),
                Change::Put(position, value) => (*position, name, value.clone()),
            })
        })
        .collect();
    puts.sort_by_key(|(position, _, _)| *position);
    out.extend(
        puts.into_iter()
            .map(|(position, name, declaration)| PlanChange::InputPut {
                name: name.clone(),
                position,
                declaration,
            }),
    );
    let mut puts: Vec<_> = delta
        .outputs
        .iter()
        .filter_map(|(name, change)| {
            Some(match change {
                Change::Removed => return None,
                Change::Moved(position) => (*position, name, base.output_row(name)?.1.clone()),
                Change::Put(position, value) => (*position, name, value.clone()),
            })
        })
        .collect();
    puts.sort_by_key(|(position, _, _)| *position);
    out.extend(
        puts.into_iter()
            .map(|(position, name, binding)| PlanChange::OutputPut {
                name: name.clone(),
                position,
                binding,
            }),
    );
    let mut puts: Vec<_> = delta
        .steps
        .iter()
        .filter_map(|(id, change)| {
            Some(match change {
                Change::Removed => return None,
                Change::Moved(position) => {
                    (*position, id, base.steps().get(id)?.declaration.clone())
                }
                Change::Put(position, value) => (*position, id, value.clone()),
            })
        })
        .collect();
    puts.sort_by_key(|(position, _, _)| *position);
    out.extend(
        puts.into_iter()
            .map(|(position, step, declaration)| PlanChange::StepPut {
                step: step.clone(),
                position,
                declaration,
            }),
    );
    out
}
fn removed<K: Clone, V>(changes: &IndexMap<K, Change<V>>) -> Vec<K> {
    changes
        .iter()
        .filter(|(_, change)| matches!(change, Change::Removed))
        .map(|(key, _)| key.clone())
        .collect()
}

/// `unit.add`'s expansion against the candidate so far: the recipe staged (params,
/// substitution, tags, suffix overrides), its ids against the candidate's, then the `after`
/// overrides on the unit's entry steps (`*`) or the named suffix's step. Nothing is compiled
/// here: the candidate is validated with everything else. Errors are under the operation's
/// field paths, without the `ops[i].` prefix.
fn expand_unit(
    work: &Work<'_>,
    recipes: &IndexMap<String, RecipeEntry>,
    signatures: &impl SignatureProvider,
    recipe: &str,
    unit: &UnitName,
    params: &JsonMap,
    options: &ExpansionOptions,
) -> Result<Vec<(StepId, JsonMap)>, Vec<PathError>> {
    let Some(entry) = recipes.get(recipe) else {
        return Err(vec![diagnostic("recipe", format!("no recipe {recipe}"))]);
    };
    let found = entry.recipe.as_ref().map_err(|errors| {
        errors
            .iter()
            .map(|e| {
                diagnostic(
                    "recipe",
                    format!("recipe {} ({}): {e}", entry.name, entry.scope),
                )
            })
            .collect::<Vec<_>>()
    })?;
    if params
        .0
        .get("unit")
        .is_some_and(|v| v.as_value().as_str() != Some(unit.as_str()))
    {
        return Err(vec![diagnostic(
            "params.unit",
            "must match the requested unit",
        )]);
    }
    let mut params = params.clone();
    params.0.insert(
        "unit".into(),
        JsonValue::try_from(Value::String(unit.to_string())).expect("a string"),
    );
    let (mut steps, by) = found.stage(&params, options, signatures)?;
    let collisions: Vec<_> = steps
        .keys()
        .filter(|id| work.has_step(id))
        .map(|id| diagnostic(&format!("steps.{id}"), "already exists in the plan"))
        .collect();
    if !collisions.is_empty() {
        return Err(collisions);
    }
    // The unit's entry steps among the new ones, its members being every step of the candidate
    // in that unit as well.
    let is_step = |name: &str| work.has_step(name) || steps.contains_key(name);
    let mut declarations: IndexMap<String, JsonMap> = work
        .members(unit.as_str())
        .into_iter()
        .map(|id| {
            let declaration = work.step(&id).expect("a member").1.clone();
            (id.to_string(), declaration)
        })
        .collect();
    declarations.extend(steps.iter().map(|(id, step)| (id.clone(), map_of(step))));
    let entries: Vec<String> = steps
        .keys()
        .filter(|id| {
            !decl_dependencies(&declarations[id.as_str()], &is_step)
                .iter()
                .any(|d| declarations.contains_key(d))
        })
        .cloned()
        .collect();
    for (suffix, gates) in &options.after {
        let targets = if suffix == "*" {
            entries.clone()
        } else {
            vec![by[suffix].clone()]
        };
        for id in targets {
            let step = steps[&id].as_object_mut().expect("a staged step");
            // A malformed `after` is kept as written for validation to refuse, and the new
            // entries are appended to it.
            let mut entries: Vec<Value> = vec![];
            let old = match step.get("after") {
                Some(Value::Array(old)) => old.clone(),
                Some(other) => vec![other.clone()],
                None => vec![],
            };
            for entry in old
                .into_iter()
                .chain(gates.iter().cloned().map(Value::String))
            {
                if !(entry.is_string() && entries.contains(&entry)) {
                    entries.push(entry);
                }
            }
            step.insert("after".into(), Value::Array(entries));
        }
    }
    steps
        .iter()
        .map(|(id, step)| {
            let id = StepId::new(id)
                .map_err(|error| vec![diagnostic(&format!("steps.{id}"), error.to_string())])?;
            Ok((id, map_of(step)))
        })
        .collect()
}
fn map_of(value: &Value) -> JsonMap {
    match value {
        Value::Object(map) => JsonMap(
            map.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        JsonValue::try_from(v.clone()).expect("strict JSON"),
                    )
                })
                .collect(),
        ),
        _ => unreachable!("a staged step is an object"),
    }
}
