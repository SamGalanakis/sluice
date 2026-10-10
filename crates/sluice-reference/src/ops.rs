//! The reference semantics of `plan_rows::PlanOp` (`docs/design/plan-rows.md` §2.4, §4, §7.6):
//! operations applied in order to one candidate held as authored rows (`PlanRows`), with no
//! compilation. Validation of the candidate is the whole-plan compiler's (`harness`).
//!
//! What a refused operation says is pinned here (the contract fixes only the paths
//! `ops[<i>].<field>` and the example `ops[1].step: no step relase`):
//!
//! | Operation | Refusal |
//! |---|---|
//! | `input.remove` | `ops[i].name: no input <name>` |
//! | `output.remove` | `ops[i].name: no output <name>` |
//! | `step.add` | `ops[i].step: step <id> already exists` |
//! | `step.update` | `ops[i].step: no step <id>` |
//! | `step.remove` | `ops[i].steps[j]: no step <id>` |
//! | `edge.add`, `edge.remove` | `ops[i].step: no step <id>`, `ops[i].step: no unit <u>` (or the unit name's own error); `ops[i].after[j]: <entry error>`, `ops[i].after[j]: no unit <u>` |
//! | `unit.add` | `ops[i].recipe: no recipe <r>`; `ops[i].recipe: recipe <r> (<scope>): <error>`; `ops[i].params.unit: must match the requested unit`; the expansion's own errors under `ops[i].` (`ops[i].params.n: required`, `ops[i].after.fork: recipe r has no step fork`, …); `ops[i].steps.<id>: already exists in the plan` |
//! | `unit.update` | `ops[i].unit: no unit <u>`; `ops[i].changes.<id>: not a step of unit <u>` |
//! | `unit.remove` | `ops[i].unit: no unit <u>` |
//! | `order.set` | `ops[i].ids[j]: no <step\|input\|output> <id>`; `ops[i].ids[j]: <id> is listed twice`; `ops[i].ids: <id> is missing` |
//!
//! An entry error is `Gate::compile`'s syntax error (`! is only allowed on boolean refs, not
//! unit entries`, `? is only allowed on step and unit entries, not refs`, a bad ref); an entry's
//! type (a ref that is not a boolean, an unknown input) is the candidate's validation, at
//! `steps.<id>.after[k]`. Every operation is checked; a refused one changes nothing, and the
//! ones after it see the candidate without it.

use crate::{
    gates::ValueRef,
    ids::{StepId, UnitName},
    plan::{SignatureProvider, diagnostic},
    recipe::{ExpansionOptions, RecipeEntry},
    rpc::{JsonMap, JsonValue},
    types::PathError,
};
use indexmap::{IndexMap, IndexSet};
use serde_json::Value;
use sluice_model::plan_rows::{
    InputRow, OrderCollection, OutputRow, PlanChange, PlanHeader, PlanOp, PlanRows, RootSection,
    StepChanges, StepRow,
};

/// The canonical order of the root sections (§2.4).
pub const CANONICAL: [RootSection; 3] = [
    RootSection::Inputs,
    RootSection::Outputs,
    RootSection::Steps,
];

/// A document's rows with positions `0 … n-1` in document order (`PlanRows::from_document`
/// with no base). Root keys other than the three sections are dropped; a step key that is not
/// a step id is an error.
pub fn rows_of(document: &JsonMap, header: PlanHeader) -> Result<PlanRows, PathError> {
    let mut rows = PlanRows {
        header: PlanHeader {
            root_order: vec![],
            ..header
        },
        inputs: vec![],
        outputs: vec![],
        steps: vec![],
    };
    for (key, value) in &document.0 {
        let section = match key.as_str() {
            "inputs" => RootSection::Inputs,
            "outputs" => RootSection::Outputs,
            "steps" => RootSection::Steps,
            _ => continue,
        };
        rows.header.root_order.push(section);
        let Some(map) = value.as_value().as_object() else {
            return Err(diagnostic(key, "expected an object"));
        };
        for (position, (name, value)) in map.iter().enumerate() {
            let position = position as u64;
            let path = format!("{key}.{name}");
            match section {
                RootSection::Inputs => rows.inputs.push(InputRow {
                    name: name.clone(),
                    position,
                    declaration: JsonValue::try_from(value.clone())
                        .map_err(|e| diagnostic(&path, e.to_string()))?,
                }),
                RootSection::Outputs => rows.outputs.push(OutputRow {
                    name: name.clone(),
                    position,
                    binding: object(value, &path)?,
                }),
                RootSection::Steps => rows.steps.push(StepRow {
                    step: StepId::new(name).map_err(|e| diagnostic(&path, e.to_string()))?,
                    position,
                    declaration: object(value, &path)?,
                }),
            }
        }
    }
    Ok(rows)
}
fn object(value: &Value, path: &str) -> Result<JsonMap, PathError> {
    match value {
        Value::Object(map) => Ok(JsonMap(
            map.iter()
                .map(|(k, v)| {
                    JsonValue::try_from(v.clone())
                        .map(|v| (k.clone(), v))
                        .map_err(|e| diagnostic(path, e.to_string()))
                })
                .collect::<Result<_, _>>()?,
        )),
        _ => Err(diagnostic(path, "expected an object")),
    }
}

/// The document the rows export (`PlanRows::to_document`): the present sections in
/// `root_order`, each collection in position order.
pub fn document_of(rows: &PlanRows) -> JsonMap {
    let mut document = JsonMap::default();
    for section in &rows.header.root_order {
        let value: serde_json::Map<String, Value> = match section {
            RootSection::Inputs => sorted(&rows.inputs, |r| r.position)
                .map(|r| (r.name.clone(), r.declaration.as_value().clone()))
                .collect(),
            RootSection::Outputs => sorted(&rows.outputs, |r| r.position)
                .map(|r| (r.name.clone(), map_value(&r.binding)))
                .collect(),
            RootSection::Steps => sorted(&rows.steps, |r| r.position)
                .map(|r| (r.step.to_string(), map_value(&r.declaration)))
                .collect(),
        };
        document.0.insert(
            section.as_str().into(),
            JsonValue::try_from(Value::Object(value)).expect("strict rows"),
        );
    }
    document
}
fn sorted<T>(rows: &[T], key: impl Fn(&T) -> u64) -> impl Iterator<Item = &T> {
    let mut rows: Vec<_> = rows.iter().collect();
    rows.sort_by_key(|r| key(r));
    rows.into_iter()
}
fn map_value(map: &JsonMap) -> Value {
    Value::Object(
        map.0
            .iter()
            .map(|(k, v)| (k.clone(), v.as_value().clone()))
            .collect(),
    )
}

/// The net row changes from `base` to `candidate` (§4, §5.1): `header.put` when the root
/// order differs; a delete for each key only in `base`; a put for each key only in
/// `candidate`, or in both with another position or a declaration whose compact serialization
/// differs. Header first, then deletes (inputs, outputs, steps) each in key order, then puts
/// (inputs, outputs, steps) each in position order.
pub fn changes(base: &PlanRows, candidate: &PlanRows) -> Vec<PlanChange> {
    let mut out = vec![];
    if base.header.root_order != candidate.header.root_order {
        out.push(PlanChange::HeaderPut {
            root_order: candidate.header.root_order.clone(),
        });
    }
    let mut deletes = |names: Vec<String>, make: &dyn Fn(String) -> PlanChange| {
        let mut names = names;
        names.sort();
        out.extend(names.into_iter().map(make));
    };
    deletes(
        removed(&base.inputs, &candidate.inputs, |r| r.name.clone()),
        &|name| PlanChange::InputDelete { name },
    );
    deletes(
        removed(&base.outputs, &candidate.outputs, |r| r.name.clone()),
        &|name| PlanChange::OutputDelete { name },
    );
    deletes(
        removed(&base.steps, &candidate.steps, |r| r.step.to_string()),
        &|name| PlanChange::StepDelete {
            step: StepId::new(name).expect("a step row's id"),
        },
    );
    for row in sorted(&candidate.inputs, |r| r.position) {
        let old = base.inputs.iter().find(|o| o.name == row.name);
        if old.is_none_or(|o| {
            o.position != row.position || compact(&o.declaration) != compact(&row.declaration)
        }) {
            out.push(PlanChange::InputPut {
                name: row.name.clone(),
                position: row.position,
                declaration: row.declaration.clone(),
            });
        }
    }
    for row in sorted(&candidate.outputs, |r| r.position) {
        let old = base.outputs.iter().find(|o| o.name == row.name);
        if old.is_none_or(|o| {
            o.position != row.position || compact(&o.binding) != compact(&row.binding)
        }) {
            out.push(PlanChange::OutputPut {
                name: row.name.clone(),
                position: row.position,
                binding: row.binding.clone(),
            });
        }
    }
    for row in sorted(&candidate.steps, |r| r.position) {
        let old = base.steps.iter().find(|o| o.step == row.step);
        if old.is_none_or(|o| {
            o.position != row.position || compact(&o.declaration) != compact(&row.declaration)
        }) {
            out.push(PlanChange::StepPut {
                step: row.step.clone(),
                position: row.position,
                declaration: row.declaration.clone(),
            });
        }
    }
    out
}
fn removed<T>(base: &[T], candidate: &[T], key: impl Fn(&T) -> String) -> Vec<String> {
    let kept: IndexSet<String> = candidate.iter().map(&key).collect();
    base.iter()
        .map(key)
        .filter(|name| !kept.contains(name))
        .collect()
}
/// Compact serialization, for declaration equality (decision 24's byte equality).
fn compact<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("JSON serializes")
}

/// What applying the operations made.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// The candidate's rows (the header's `rev` and `state_epoch` are the base's).
    pub rows: PlanRows,
    /// The reply's `steps`: the ids the `step.add` and `unit.add` operations added, in
    /// operation order (a unit's in recipe order); `None` when there are none.
    pub added: Option<Vec<StepId>>,
}

/// One collection of the working candidate: key → (position, declaration).
type Collection<V> = IndexMap<String, (u64, V)>;

struct Working {
    root_order: Vec<RootSection>,
    inputs: Collection<JsonValue>,
    outputs: Collection<JsonMap>,
    steps: Collection<JsonMap>,
}
impl Working {
    fn of(rows: &PlanRows) -> Self {
        Self {
            root_order: rows.header.root_order.clone(),
            inputs: rows
                .inputs
                .iter()
                .map(|r| (r.name.clone(), (r.position, r.declaration.clone())))
                .collect(),
            outputs: rows
                .outputs
                .iter()
                .map(|r| (r.name.clone(), (r.position, r.binding.clone())))
                .collect(),
            steps: rows
                .steps
                .iter()
                .map(|r| (r.step.to_string(), (r.position, r.declaration.clone())))
                .collect(),
        }
    }
    fn rows(self, header: &PlanHeader) -> PlanRows {
        fn ordered<V>(c: Collection<V>) -> Vec<(String, u64, V)> {
            let mut rows: Vec<_> = c.into_iter().map(|(k, (p, v))| (k, p, v)).collect();
            rows.sort_by_key(|(_, p, _)| *p);
            rows
        }
        PlanRows {
            header: PlanHeader {
                root_order: self.root_order,
                ..header.clone()
            },
            inputs: ordered(self.inputs)
                .into_iter()
                .map(|(name, position, declaration)| InputRow {
                    name,
                    position,
                    declaration,
                })
                .collect(),
            outputs: ordered(self.outputs)
                .into_iter()
                .map(|(name, position, binding)| OutputRow {
                    name,
                    position,
                    binding,
                })
                .collect(),
            steps: ordered(self.steps)
                .into_iter()
                .map(|(step, position, declaration)| StepRow {
                    step: StepId::new(step).expect("checked step id"),
                    position,
                    declaration,
                })
                .collect(),
        }
    }
    /// A put into an absent section adds it before the first present section that follows
    /// it canonically, else at the end (§2.4).
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
    fn has_step(&self, id: &str) -> bool {
        self.steps.contains_key(id)
    }
    /// Each unit's members, in position order: a step's unit is its first `unit:` tag, else
    /// its id (`steps.unit`, §2.5).
    fn units(&self) -> IndexMap<String, Vec<String>> {
        let mut steps: Vec<_> = self.steps.iter().collect();
        steps.sort_by_key(|(_, (p, _))| *p);
        let mut units = IndexMap::<String, Vec<String>>::new();
        for (id, (_, declaration)) in steps {
            units
                .entry(unit_of(id, declaration))
                .or_default()
                .push(id.clone());
        }
        units
    }
    /// A unit's entry steps (`units::derive_units`, from declarations): members none of whose
    /// bindings or gates names another member.
    fn entries(&self, members: &[String]) -> Vec<String> {
        members
            .iter()
            .filter(|id| {
                let declaration = &self.steps[id.as_str()].1;
                !dependencies(declaration, &|name| self.has_step(name))
                    .iter()
                    .any(|d| members.contains(d))
            })
            .cloned()
            .collect()
    }
    fn next_position<V>(collection: &Collection<V>) -> u64 {
        collection.values().map(|(p, _)| p + 1).max().unwrap_or(0)
    }
}

/// A step's unit: the part after `unit:` of its first `unit:` tag, else its id.
pub fn unit_of(id: &str, declaration: &JsonMap) -> String {
    declaration
        .0
        .get("tags")
        .and_then(|t| t.as_value().as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|tag| tag.strip_prefix("unit:"))
        .unwrap_or(id)
        .to_owned()
}
/// The steps a declaration depends on, read from its bindings' refs and its gate entries
/// as the compiler classifies them (`is_step` names the plan's steps); unit gates are left
/// out (a unit's exits are never another unit's members).
pub fn dependencies(declaration: &JsonMap, is_step: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out = IndexSet::new();
    let step_of = |text: &str| {
        ValueRef(text.to_owned())
            .parts()
            .ok()
            .and_then(|r| r.step)
            .map(|s| s.to_string())
    };
    if let Some(bindings) = declaration
        .0
        .get("in")
        .and_then(|b| b.as_value().as_object())
    {
        for binding in bindings.values() {
            match binding.get("source") {
                Some(Value::String(text)) => out.extend(step_of(text)),
                Some(Value::Array(items)) => {
                    out.extend(items.iter().filter_map(Value::as_str).filter_map(step_of))
                }
                _ => {}
            }
        }
    }
    for entry in declaration
        .0
        .get("after")
        .and_then(|a| a.as_value().as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let (negate, entry) = match entry.strip_prefix('!') {
            Some(entry) => (true, entry),
            None => (false, entry),
        };
        let (accept_skip, entry) = match entry.strip_suffix('?') {
            Some(entry) => (true, entry),
            None => (false, entry),
        };
        if entry.starts_with("unit:") {
            continue;
        }
        if StepId::new(entry).is_ok() && is_step(entry) {
            if !negate {
                out.insert(entry.to_owned());
            }
            continue;
        }
        if !accept_skip {
            out.extend(step_of(entry));
        }
    }
    out.into_iter().collect()
}

/// `Gate::compile`'s refusal of an entry, without its type check: the entry's syntax, how it
/// classifies (a step of the candidate, a unit, a ref), and that a named unit exists.
fn entry_error(
    text: &str,
    is_step: &dyn Fn(&str) -> bool,
    units: &IndexMap<String, Vec<String>>,
) -> Option<String> {
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
            return Some("! is only allowed on boolean refs, not unit entries".into());
        }
        return match UnitName::new(name) {
            Err(error) => Some(error.to_string()),
            Ok(_) if !units.contains_key(name) => Some(format!("no unit {name}")),
            Ok(_) => None,
        };
    }
    if StepId::new(entry).is_ok() && is_step(entry) {
        return negate.then(|| "! is only allowed on boolean refs, not step entries".into());
    }
    if accept_skip {
        return Some("? is only allowed on step and unit entries, not refs".into());
    }
    ValueRef::parse(entry).err()
}

/// Apply `step.update`'s changes to a declaration: a given field replaces that key (an
/// existing key keeps its place, a new one is appended), null removes it.
pub fn update(declaration: &mut JsonMap, changes: &StepChanges) {
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
/// (`ops[i]…`); none means the candidate, to be validated whole. `start: false` adds
/// `"paused": true` (appended) to every step the edit adds by net effect whose declaration
/// has no `paused`.
pub fn apply(
    base: &PlanRows,
    ops: &[PlanOp],
    start: bool,
    recipes: &IndexMap<String, RecipeEntry>,
    signatures: &impl SignatureProvider,
) -> Result<Applied, Vec<PathError>> {
    let mut work = Working::of(base);
    let mut errors = vec![];
    let mut added: Vec<StepId> = vec![];
    for (index, op) in ops.iter().enumerate() {
        let at = |field: &str| format!("ops[{index}].{field}");
        let mut refused =
            |field: &str, message: String| errors.push(diagnostic(&at(field), message));
        match op {
            PlanOp::InputPut { name, declaration } => {
                work.ensure(RootSection::Inputs);
                let next = Working::next_position(&work.inputs);
                let entry = work
                    .inputs
                    .entry(name.clone())
                    .or_insert((next, declaration.clone()));
                entry.1 = declaration.clone();
            }
            PlanOp::InputRemove { name } => {
                if work.inputs.shift_remove(name).is_none() {
                    refused("name", format!("no input {name}"));
                }
            }
            PlanOp::OutputPut { name, source } => {
                work.ensure(RootSection::Outputs);
                let binding = JsonMap(IndexMap::from([(
                    "source".to_owned(),
                    JsonValue::try_from(Value::String(source.clone())).expect("a string"),
                )]));
                let next = Working::next_position(&work.outputs);
                let entry = work
                    .outputs
                    .entry(name.clone())
                    .or_insert((next, binding.clone()));
                entry.1 = binding;
            }
            PlanOp::OutputRemove { name } => {
                if work.outputs.shift_remove(name).is_none() {
                    refused("name", format!("no output {name}"));
                }
            }
            PlanOp::StepAdd { step, spec } => {
                if work.has_step(step.as_str()) {
                    refused("step", format!("step {step} already exists"));
                    continue;
                }
                work.ensure(RootSection::Steps);
                let next = Working::next_position(&work.steps);
                work.steps.insert(step.to_string(), (next, spec.clone()));
                added.push(step.clone());
            }
            PlanOp::StepUpdate { step, changes } => match work.steps.get_mut(step.as_str()) {
                Some((_, declaration)) => update(declaration, changes),
                None => refused("step", format!("no step {step}")),
            },
            PlanOp::StepRemove { steps } => {
                let mut found = vec![];
                for (j, id) in steps.iter().enumerate() {
                    if work.has_step(id.as_str()) {
                        found.push(id.as_str());
                    } else {
                        refused(&format!("steps[{j}]"), format!("no step {id}"));
                    }
                }
                if found.len() == steps.len() {
                    for id in found {
                        work.steps.shift_remove(id);
                    }
                }
            }
            PlanOp::EdgeAdd { step, after } | PlanOp::EdgeRemove { step, after } => {
                let adding = matches!(op, PlanOp::EdgeAdd { .. });
                let units = work.units();
                let is_step = |name: &str| work.has_step(name);
                let mut refusals = vec![];
                let targets = match step.strip_prefix("unit:") {
                    Some(name) => match UnitName::new(name) {
                        Err(error) => {
                            refusals.push(("step".to_owned(), error.to_string()));
                            vec![]
                        }
                        Ok(_) => match units.get(name) {
                            Some(members) => work.entries(members),
                            None => {
                                refusals.push(("step".to_owned(), format!("no unit {name}")));
                                vec![]
                            }
                        },
                    },
                    None if is_step(step) => vec![step.clone()],
                    None => {
                        refusals.push(("step".to_owned(), format!("no step {step}")));
                        vec![]
                    }
                };
                for (j, entry) in after.iter().enumerate() {
                    if let Some(error) = entry_error(entry, &is_step, &units) {
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
                    let declaration = &mut work.steps.get_mut(&id).expect("a target").1;
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
                    if new.is_empty() {
                        declaration.0.shift_remove("after");
                    } else {
                        declaration.0.insert(
                            "after".into(),
                            JsonValue::try_from(serde_json::json!(new)).expect("strings"),
                        );
                    }
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
                        let next = Working::next_position(&work.steps);
                        work.steps.insert(id.clone(), (next, declaration));
                        added.push(StepId::new(id).expect("checked step id"));
                    }
                }
            },
            PlanOp::UnitUpdate { unit, changes } => {
                let units = work.units();
                let Some(members) = units.get(unit.as_str()) else {
                    refused("unit", format!("no unit {unit}"));
                    continue;
                };
                let strangers: Vec<_> = changes
                    .keys()
                    .filter(|id| !members.iter().any(|m| m == id.as_str()))
                    .collect();
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
                    update(
                        &mut work.steps.get_mut(id.as_str()).expect("a member").1,
                        change,
                    );
                }
            }
            PlanOp::UnitRemove { unit } => match work.units().get(unit.as_str()) {
                Some(members) => {
                    for id in members {
                        work.steps.shift_remove(id);
                    }
                }
                None => refused("unit", format!("no unit {unit}")),
            },
            PlanOp::OrderSet { collection, ids } => {
                let (members, noun): (Vec<String>, &str) = match collection {
                    OrderCollection::Steps => (work.steps.keys().cloned().collect(), "step"),
                    OrderCollection::Inputs => (work.inputs.keys().cloned().collect(), "input"),
                    OrderCollection::Outputs => (work.outputs.keys().cloned().collect(), "output"),
                };
                let mut seen = IndexSet::new();
                let mut bad = false;
                for (j, id) in ids.iter().enumerate() {
                    if !members.contains(id) {
                        refused(&format!("ids[{j}]"), format!("no {noun} {id}"));
                        bad = true;
                    } else if !seen.insert(id.as_str()) {
                        refused(&format!("ids[{j}]"), format!("{id} is listed twice"));
                        bad = true;
                    }
                }
                let mut missing: Vec<_> = match collection {
                    OrderCollection::Steps => positioned(&work.steps),
                    OrderCollection::Inputs => positioned(&work.inputs),
                    OrderCollection::Outputs => positioned(&work.outputs),
                };
                missing.retain(|id| !seen.contains(id.as_str()));
                for id in missing {
                    refused("ids", format!("{id} is missing"));
                    bad = true;
                }
                if bad {
                    continue;
                }
                fn renumber<V>(c: &mut Collection<V>, ids: &[String]) {
                    for (position, id) in ids.iter().enumerate() {
                        c.get_mut(id).expect("a member").0 = position as u64;
                    }
                }
                match collection {
                    OrderCollection::Steps => renumber(&mut work.steps, ids),
                    OrderCollection::Inputs => renumber(&mut work.inputs, ids),
                    OrderCollection::Outputs => renumber(&mut work.outputs, ids),
                }
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    if !start {
        let before: IndexSet<String> = base.steps.iter().map(|r| r.step.to_string()).collect();
        for (id, (_, declaration)) in &mut work.steps {
            if !before.contains(id) && !declaration.0.contains_key("paused") {
                declaration.0.insert(
                    "paused".into(),
                    JsonValue::try_from(Value::Bool(true)).expect("bool"),
                );
            }
        }
    }
    Ok(Applied {
        rows: work.rows(&base.header),
        added: (!added.is_empty()).then_some(added),
    })
}
fn positioned<V>(collection: &Collection<V>) -> Vec<String> {
    let mut keys: Vec<_> = collection
        .iter()
        .map(|(k, (p, _))| (*p, k.clone()))
        .collect();
    keys.sort();
    keys.into_iter().map(|(_, k)| k).collect()
}

/// `unit.add`'s expansion against the candidate so far: today's `Recipe::expand` up to its
/// staging (params, substitution, tags, suffix overrides), the new ids against the
/// candidate's, then the `after` overrides on the unit's entry steps (`*`) or the named
/// suffix. No compilation: the candidate is validated whole afterwards. Errors are under
/// `ops[i].`'s field paths, without the prefix.
fn expand_unit(
    work: &Working,
    recipes: &IndexMap<String, RecipeEntry>,
    signatures: &impl SignatureProvider,
    recipe: &str,
    unit: &UnitName,
    params: &JsonMap,
    options: &ExpansionOptions,
) -> Result<Vec<(String, JsonMap)>, Vec<PathError>> {
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
    // the unit's entry steps among the new ones, its members being every step of the
    // candidate in that unit as well (as today's staging compile derives them)
    let is_step = |name: &str| work.has_step(name) || steps.contains_key(name);
    let declarations: IndexMap<String, JsonMap> = work
        .units()
        .get(unit.as_str())
        .into_iter()
        .flatten()
        .map(|id| (id.clone(), work.steps[id.as_str()].1.clone()))
        .chain(steps.iter().map(|(id, step)| (id.clone(), map_of(step))))
        .collect();
    let entries: Vec<String> = steps
        .keys()
        .filter(|id| {
            !dependencies(&declarations[id.as_str()], &is_step)
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
            let step = steps[&id].as_object_mut().expect("checked step");
            // today's `expand` reads a validated `after`; here a malformed one is kept as
            // written for validation to refuse, and the new entries are appended to it
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
    Ok(steps
        .iter()
        .map(|(id, step)| (id.clone(), map_of(step)))
        .collect())
}
fn map_of(value: &Value) -> JsonMap {
    object(value, "").expect("a strict step object")
}

/// `base` with `changes` applied as the store applies a commit (§4 step 5): `header.put` sets
/// the root order, deletes remove rows, puts insert or replace `(position, declaration)`.
/// Applying an edit's `changes` to its base gives its candidate's rows.
pub fn apply_changes(base: &PlanRows, changes: &[PlanChange]) -> PlanRows {
    let mut work = Working::of(base);
    for change in changes {
        match change {
            PlanChange::HeaderPut { root_order } => work.root_order = root_order.clone(),
            PlanChange::InputDelete { name } => {
                work.inputs.shift_remove(name);
            }
            PlanChange::OutputDelete { name } => {
                work.outputs.shift_remove(name);
            }
            PlanChange::StepDelete { step } => {
                work.steps.shift_remove(step.as_str());
            }
            PlanChange::InputPut {
                name,
                position,
                declaration,
            } => {
                work.inputs
                    .insert(name.clone(), (*position, declaration.clone()));
            }
            PlanChange::OutputPut {
                name,
                position,
                binding,
            } => {
                work.outputs
                    .insert(name.clone(), (*position, binding.clone()));
            }
            PlanChange::StepPut {
                step,
                position,
                declaration,
            } => {
                work.steps
                    .insert(step.to_string(), (*position, declaration.clone()));
            }
        }
    }
    work.rows(&base.header)
}
