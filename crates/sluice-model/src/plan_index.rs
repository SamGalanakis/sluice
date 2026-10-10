//! The plan's rebuildable index rows (`docs/design/plan-rows.md` §2.5), derived from
//! declarations alone: no fn manifest is read, so the converter, `verify` and an edit's
//! preparation derive the same rows. `steps.unit`, `step_tags` and `plan_refs` come from one
//! declaration (given which bare names are steps); `plan_edges` need the whole plan's units.

use crate::{
    gates::{Reference, ValueRef},
    ids::{StepId, UnitName},
    plan_rows::{
        ConsumerKind, EdgeKind, EdgeRow, PlanRows, RefKind, ReferenceRow, SourceKind, StepIndexRows,
    },
    rpc::JsonMap,
};
use indexmap::{IndexMap, IndexSet};
use serde_json::Value;
use std::collections::HashSet;

/// A step's unit name, tags and references (`(slot, ordinal)` order). The unit is the part
/// after `unit:` of the first `unit:` tag, else the step id. `is_step` says which bare gate
/// names are steps; a bare name that is not one reads as a plan input (inputs and steps share
/// one namespace), and an entry that parses as neither makes no row.
pub fn step_index(
    step: &StepId,
    declaration: &JsonMap,
    is_step: &dyn Fn(&str) -> bool,
) -> StepIndexRows {
    let tags: Vec<String> = declaration
        .0
        .get("tags")
        .and_then(|tags| tags.as_value().as_array())
        .map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<IndexSet<_>>()
                .into_iter()
                .collect()
        })
        .unwrap_or_default();
    let unit = tags
        .iter()
        .find_map(|tag| tag.strip_prefix("unit:"))
        .and_then(|name| UnitName::new(name).ok())
        .unwrap_or_else(|| UnitName::new(step.as_str()).expect("a step id is a unit name"));
    let row = |slot: &str, ordinal: usize, kind: RefKind, source: Source| ReferenceRow {
        consumer_kind: ConsumerKind::Step,
        consumer_id: step.to_string(),
        slot: slot.to_owned(),
        ordinal: ordinal as u32,
        kind,
        source_kind: source.kind,
        source_id: source.id,
        source_port: source.port,
        source_path: source.path,
    };
    let mut references = vec![];
    if let Some(Value::Object(bindings)) = declaration.0.get("in").map(|b| b.as_value()) {
        for (input, binding) in bindings {
            let slot = format!("in.{input}");
            match binding.get("source") {
                Some(Value::String(text)) => {
                    if let Some(source) = reference(text) {
                        references.push(row(&slot, 0, RefKind::Binding, source));
                    }
                }
                Some(Value::Array(texts)) => {
                    for (ordinal, text) in texts.iter().enumerate() {
                        if let Some(source) = text.as_str().and_then(reference) {
                            references.push(row(&slot, ordinal, RefKind::Binding, source));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(Value::Array(after)) = declaration.0.get("after").map(|a| a.as_value()) {
        for (ordinal, entry) in after.iter().enumerate() {
            if let Some(source) = entry.as_str().and_then(|entry| gate(entry, is_step)) {
                references.push(row("after", ordinal, RefKind::Gate, source));
            }
        }
    }
    references.sort_by(|a, b| (&a.slot, a.ordinal).cmp(&(&b.slot, b.ordinal)));
    StepIndexRows {
        step: step.clone(),
        unit,
        tags,
        references,
    }
}

/// A plan output's single reference: consumer `output`, slot `source`, ordinal 0, kind
/// `output`. None when its binding has no readable `source`.
pub fn output_references(name: &str, binding: &JsonMap) -> Vec<ReferenceRow> {
    binding
        .0
        .get("source")
        .and_then(|source| source.as_value().as_str())
        .and_then(reference)
        .map(|source| ReferenceRow {
            consumer_kind: ConsumerKind::Output,
            consumer_id: name.to_owned(),
            slot: "source".into(),
            ordinal: 0,
            kind: RefKind::Output,
            source_kind: source.kind,
            source_id: source.id,
            source_port: source.port,
            source_path: source.path,
        })
        .into_iter()
        .collect()
}

/// Every `plan_edges` row of a plan: for each step (target), one `data` edge per distinct step
/// its bindings read, one `gate` edge per distinct step its step gates and boolean gates read,
/// and for each `unit:u` gate one `gate` edge from each current exit step of `u` with
/// `via_unit` u. Sources that are not current steps make no edge. These are
/// `plan::expanded_dependencies`, split by kind; targets in position order.
pub fn plan_edges(rows: &PlanRows) -> Vec<EdgeRow> {
    let ids: HashSet<&str> = rows.steps.iter().map(|row| row.step.as_str()).collect();
    let is_step = |name: &str| ids.contains(name);
    let indexes: Vec<StepIndexRows> = rows
        .steps
        .iter()
        .map(|row| step_index(&row.step, &row.declaration, &is_step))
        .collect();
    let exits = unit_exits(rows, &indexes);
    let mut edges = vec![];
    for index in &indexes {
        edges.extend(incoming_edges(index, &ids, &exits));
    }
    edges
}

/// One target's incoming edges, given the current steps and each unit's exit steps.
pub fn incoming_edges(
    index: &StepIndexRows,
    steps: &HashSet<&str>,
    exits: &IndexMap<UnitName, Vec<StepId>>,
) -> Vec<EdgeRow> {
    let (data, gates) = step_sources(index, steps);
    let edge = |source: StepId, kind: EdgeKind, via_unit: Option<UnitName>| EdgeRow {
        source,
        target: index.step.clone(),
        kind,
        via_unit,
    };
    let mut edges: Vec<EdgeRow> = data
        .into_iter()
        .map(|source| edge(source, EdgeKind::Data, None))
        .chain(
            gates
                .into_iter()
                .map(|source| edge(source, EdgeKind::Gate, None)),
        )
        .collect();
    let mut units = IndexSet::new();
    for reference in &index.references {
        if reference.kind == RefKind::Gate && reference.source_kind == SourceKind::Unit {
            units.insert(reference.source_id.clone());
        }
    }
    for unit in units {
        let Ok(name) = UnitName::new(unit.as_str()) else {
            continue;
        };
        for exit in exits.get(&name).into_iter().flatten() {
            edges.push(edge(exit.clone(), EdgeKind::Gate, Some(name.clone())));
        }
    }
    edges
}

/// Each unit's exit steps, units in first-member order: its members tagged `exit`, else its
/// sinks over the edges inside the unit (`units::derive_units`).
pub fn unit_exits(rows: &PlanRows, indexes: &[StepIndexRows]) -> IndexMap<UnitName, Vec<StepId>> {
    let ids: HashSet<&str> = rows.steps.iter().map(|row| row.step.as_str()).collect();
    let mut members: IndexMap<UnitName, Vec<&StepIndexRows>> = IndexMap::new();
    for index in indexes {
        members.entry(index.unit.clone()).or_default().push(index);
    }
    members
        .into_iter()
        .map(|(unit, steps)| {
            let inside: HashSet<&str> = steps.iter().map(|s| s.step.as_str()).collect();
            let tagged: Vec<StepId> = steps
                .iter()
                .filter(|s| s.tags.iter().any(|tag| tag == "exit"))
                .map(|s| s.step.clone())
                .collect();
            let exits = if tagged.is_empty() {
                let mut used = HashSet::new();
                for step in &steps {
                    let (data, gates) = step_sources(step, &ids);
                    used.extend(
                        data.into_iter()
                            .chain(gates)
                            .filter(|source| inside.contains(source.as_str())),
                    );
                }
                steps
                    .iter()
                    .filter(|s| !used.contains(&s.step))
                    .map(|s| s.step.clone())
                    .collect()
            } else {
                tagged
            };
            (unit, exits)
        })
        .collect()
}

/// The distinct current steps a step's bindings read, and those its step and boolean gates
/// read, each in reference order.
fn step_sources(index: &StepIndexRows, steps: &HashSet<&str>) -> (Vec<StepId>, Vec<StepId>) {
    let mut data = IndexSet::new();
    let mut gates = IndexSet::new();
    for reference in index.references.iter() {
        if reference.source_kind != SourceKind::Step
            || !steps.contains(reference.source_id.as_str())
        {
            continue;
        }
        let Ok(id) = StepId::new(reference.source_id.as_str()) else {
            continue;
        };
        match reference.kind {
            RefKind::Binding => {
                data.insert(id);
            }
            RefKind::Gate => {
                gates.insert(id);
            }
            RefKind::Output => {}
        }
    }
    (data.into_iter().collect(), gates.into_iter().collect())
}

struct Source {
    kind: SourceKind,
    id: String,
    port: String,
    path: String,
}

/// `<input>[.<path>]` or `<step>/<output>[.<path>]`.
fn reference(text: &str) -> Option<Source> {
    let Reference { step, name, fields } = ValueRef(text.to_owned()).parts().ok()?;
    Some(match step {
        None => Source {
            kind: SourceKind::Input,
            id: name,
            port: String::new(),
            path: fields.join("."),
        },
        Some(step) => Source {
            kind: SourceKind::Step,
            id: step.to_string(),
            port: name,
            path: fields.join("."),
        },
    })
}

/// An `after` entry: `unit:u`/`unit:u?`, a step `s`/`s?`, or a boolean ref `r`/`!r`.
fn gate(entry: &str, is_step: &dyn Fn(&str) -> bool) -> Option<Source> {
    let (negate, entry) = match entry.strip_prefix('!') {
        Some(entry) => (true, entry),
        None => (false, entry),
    };
    let (accept_skip, entry) = match entry.strip_suffix('?') {
        Some(entry) => (true, entry),
        None => (false, entry),
    };
    if negate && accept_skip {
        return None;
    }
    if let Some(name) = entry.strip_prefix("unit:") {
        return (!negate && UnitName::new(name).is_ok()).then(|| Source {
            kind: SourceKind::Unit,
            id: name.to_owned(),
            port: String::new(),
            path: String::new(),
        });
    }
    if !negate && is_step(entry) {
        return Some(Source {
            kind: SourceKind::Step,
            id: entry.to_owned(),
            port: String::new(),
            path: String::new(),
        });
    }
    if accept_skip {
        return None;
    }
    reference(entry)
}
