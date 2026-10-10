//! What a declaration says about the rest of the plan, read from the declaration alone (no fn
//! manifest): its unit, the names it reads, and a compiled step's `plan_edges`. The other index
//! rows (`steps.unit`, `step_tags`, `plan_refs`) are `crate::plan_index`'s, re-exported from
//! `plan`; `step_edges` agrees with its `plan_edges` (a test holds it).

use super::Plan;
use crate::{
    gates::{Gate, Reference, ValueRef},
    ids::{StepId, UnitName},
    plan_rows::{EdgeKind, EdgeRow},
    rpc::JsonMap,
};
use indexmap::IndexSet;
use serde_json::Value;

/// A step's unit: the part after `unit:` of its first `unit:` tag, else its id.
pub(crate) fn unit_of(id: &str, declaration: &JsonMap) -> String {
    tags(declaration)
        .find_map(|tag| tag.strip_prefix("unit:"))
        .unwrap_or(id)
        .to_owned()
}
fn tags(declaration: &JsonMap) -> impl Iterator<Item = &str> {
    declaration
        .0
        .get("tags")
        .and_then(|tags| tags.as_value().as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}
/// The source refs of a declaration's bindings: `(input, ordinal, ref text)`, a fan-in list's
/// in list order.
fn binding_refs(declaration: &JsonMap) -> Vec<(&str, u32, &str)> {
    let mut out = vec![];
    if let Some(bindings) = declaration
        .0
        .get("in")
        .and_then(|bindings| bindings.as_value().as_object())
    {
        for (input, binding) in bindings {
            match binding.get("source") {
                Some(Value::String(text)) => out.push((input.as_str(), 0, text.as_str())),
                Some(Value::Array(items)) => {
                    for (index, item) in items.iter().enumerate() {
                        if let Some(text) = item.as_str() {
                            out.push((input.as_str(), index as u32, text));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}
fn gate_entries(declaration: &JsonMap) -> impl Iterator<Item = (usize, &str)> {
    declaration
        .0
        .get("after")
        .and_then(|after| after.as_value().as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, entry)| Some((index, entry.as_str()?)))
}
/// A gate entry's parts: whether it is negated (`!`), accepts a skip (`?`), and the rest.
fn entry_parts(text: &str) -> (bool, bool, &str) {
    let (negate, entry) = match text.strip_prefix('!') {
        Some(entry) => (true, entry),
        None => (false, text),
    };
    let (accept_skip, entry) = match entry.strip_suffix('?') {
        Some(entry) => (true, entry),
        None => (false, entry),
    };
    (negate, accept_skip, entry)
}
fn step_of(text: &str) -> Option<String> {
    ValueRef(text.to_owned())
        .parts()
        .ok()
        .and_then(|parts| parts.step)
        .map(|step| step.to_string())
}

/// The steps a declaration depends on, from its bindings' refs and its gate entries as the
/// compiler classifies them (`is_step` names the plan's steps); unit gates are left out (a
/// unit's exits are never another unit's members).
pub(crate) fn decl_dependencies(
    declaration: &JsonMap,
    is_step: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = IndexSet::new();
    for (_, _, text) in binding_refs(declaration) {
        out.extend(step_of(text));
    }
    for (_, text) in gate_entries(declaration) {
        let (negate, accept_skip, entry) = entry_parts(text);
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
pub(crate) fn entry_error(
    text: &str,
    is_step: &dyn Fn(&str) -> bool,
    is_unit: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let (negate, accept_skip, entry) = entry_parts(text);
    if let Some(name) = entry.strip_prefix("unit:") {
        if negate {
            return Some("! is only allowed on boolean refs, not unit entries".into());
        }
        return match UnitName::new(name) {
            Err(error) => Some(error.to_string()),
            Ok(_) if !is_unit(name) => Some(format!("no unit {name}")),
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

/// The names a declaration reads (a ref's step or input; a bare gate entry), and the units it
/// gates on: the keys of the plan's reverse indexes.
pub(crate) fn referenced_names(declaration: &JsonMap) -> (IndexSet<String>, IndexSet<UnitName>) {
    let mut names = IndexSet::new();
    let mut units = IndexSet::new();
    let name_of = |text: &str| {
        ValueRef(text.to_owned())
            .parts()
            .ok()
            .map(|parts| parts.step.map_or(parts.name, |step| step.to_string()))
    };
    for (_, _, text) in binding_refs(declaration) {
        names.extend(name_of(text));
    }
    for (_, text) in gate_entries(declaration) {
        let (_, _, entry) = entry_parts(text);
        match entry.strip_prefix("unit:") {
            Some(unit) => units.extend(UnitName::new(unit).ok()),
            None => names.extend(name_of(entry)),
        }
    }
    (names, units)
}

/// A step's incoming `plan_edges` in a compiled plan: one `data` edge per distinct step its
/// bindings read, one `gate` edge per distinct step a step gate or a boolean gate names, and
/// for each unit gate one `gate` edge from each of the unit's exits, `via_unit` that unit. These
/// are its `dependencies`, split by kind. Empty for a step the plan lacks.
pub fn step_edges(plan: &Plan, id: &StepId) -> Vec<EdgeRow> {
    let Some(step) = plan.steps().get(id) else {
        return vec![];
    };
    let mut edges: IndexSet<(StepId, EdgeKind, Option<UnitName>)> = IndexSet::new();
    for source in step.data_dependencies() {
        if plan.steps().contains_key(&source) {
            edges.insert((source, EdgeKind::Data, None));
        }
    }
    for gate in &step.after {
        match gate {
            Gate::Step { id, .. } => {
                if plan.steps().contains_key(id) {
                    edges.insert((id.clone(), EdgeKind::Gate, None));
                }
            }
            Gate::Bool { reference, .. } => {
                if let Ok(Reference {
                    step: Some(source), ..
                }) = reference.parts()
                    && plan.steps().contains_key(&source)
                {
                    edges.insert((source, EdgeKind::Gate, None));
                }
            }
            Gate::Unit { name, .. } => {
                for exit in plan
                    .units()
                    .get(name)
                    .into_iter()
                    .flat_map(|unit| &unit.exits)
                {
                    edges.insert((exit.clone(), EdgeKind::Gate, Some(name.clone())));
                }
            }
        }
    }
    edges
        .into_iter()
        .map(|(source, kind, via_unit)| EdgeRow {
            source,
            target: id.clone(),
            kind,
            via_unit,
        })
        .collect()
}
