//! Lane H's independent reading of the schema-3 plan rows (`docs/design/plan-rows.md`): the
//! index rows §2.5 derives from declarations, a plan rebuilt from its logged changes (§5.3)
//! and exported (§7.2), the schema-equivalence check (§2.1), a logical dump that says whether
//! a database changed, and query plans. Written from the contract, never from the lanes'
//! code, so a test that compares the two catches either one drifting.
#![allow(dead_code)]

use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};
use sluice_model::{
    ids::StepId,
    plan_rows::{ConsumerKind, PlanChange, RefKind, ReferenceRow, RootSection, SourceKind},
};
use std::collections::{BTreeMap, BTreeSet};

// ---- §2.5: index rows from declarations alone ------------------------------------------

/// `steps.unit`: the part after `unit:` of the first `unit:` tag, else the step id.
pub fn unit_of(step: &str, declaration: &Map<String, Value>) -> String {
    tags_of(declaration)
        .iter()
        .find_map(|tag| tag.strip_prefix("unit:").map(str::to_owned))
        .unwrap_or_else(|| step.to_owned())
}

/// `step_tags`: the distinct tags, in declaration order.
pub fn tags_of(declaration: &Map<String, Value>) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for tag in declaration
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_owned());
        }
    }
    tags
}

/// A ref's source: `<input>[.<path>]` or `<step>/<output>[.<path>]`. `None` when it is not
/// a ref at all.
fn source(text: &str) -> Option<(SourceKind, String, String, String)> {
    let (head, path) = match text.split_once('.') {
        Some((head, path)) => (head, path),
        None => (text, ""),
    };
    if head.is_empty() || path.split('.').any(|f| !path.is_empty() && f.is_empty()) {
        return None;
    }
    Some(match head.split_once('/') {
        Some((step, port)) if !step.is_empty() && !port.is_empty() => (
            SourceKind::Step,
            step.to_owned(),
            port.to_owned(),
            path.to_owned(),
        ),
        Some(_) => return None,
        None => (
            SourceKind::Input,
            head.to_owned(),
            String::new(),
            path.to_owned(),
        ),
    })
}

fn row(
    consumer_kind: ConsumerKind,
    consumer: &str,
    slot: &str,
    ordinal: u32,
    kind: RefKind,
    (source_kind, source_id, source_port, source_path): (SourceKind, String, String, String),
) -> ReferenceRow {
    ReferenceRow {
        consumer_kind,
        consumer_id: consumer.to_owned(),
        slot: slot.to_owned(),
        ordinal,
        kind,
        source_kind,
        source_id,
        source_port,
        source_path,
    }
}

/// A gate entry's source, if the model can classify it: `unit:u` and `unit:u?` name a unit,
/// `s` and `s?` a step (port and path empty), `r` and `!r` a ref. A negated unit or step
/// entry, or a `?` on a ref, is no gate and makes no row.
fn gate_source(
    entry: &str,
    is_step: &dyn Fn(&str) -> bool,
) -> Option<(SourceKind, String, String, String)> {
    let (negate, entry) = match entry.strip_prefix('!') {
        Some(entry) => (true, entry),
        None => (false, entry),
    };
    let (optional, entry) = match entry.strip_suffix('?') {
        Some(entry) => (true, entry),
        None => (false, entry),
    };
    if let Some(unit) = entry.strip_prefix("unit:") {
        return (!negate && !unit.is_empty()).then(|| {
            (
                SourceKind::Unit,
                unit.to_owned(),
                String::new(),
                String::new(),
            )
        });
    }
    if is_step(entry) {
        return (!negate).then(|| {
            (
                SourceKind::Step,
                entry.to_owned(),
                String::new(),
                String::new(),
            )
        });
    }
    if optional {
        return None;
    }
    source(entry)
}

/// A step's `plan_refs`, in `(slot, ordinal)` order.
pub fn step_references(
    step: &str,
    declaration: &Map<String, Value>,
    is_step: &dyn Fn(&str) -> bool,
) -> Vec<ReferenceRow> {
    let mut rows = Vec::new();
    if let Some(bindings) = declaration.get("in").and_then(Value::as_object) {
        for (input, binding) in bindings {
            let slot = format!("in.{input}");
            match binding.get("source") {
                Some(Value::String(text)) => rows.extend(
                    source(text)
                        .map(|s| row(ConsumerKind::Step, step, &slot, 0, RefKind::Binding, s)),
                ),
                Some(Value::Array(list)) => {
                    for (ordinal, text) in list.iter().enumerate() {
                        if let Some(s) = text.as_str().and_then(source) {
                            rows.push(row(
                                ConsumerKind::Step,
                                step,
                                &slot,
                                ordinal as u32,
                                RefKind::Binding,
                                s,
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(after) = declaration.get("after").and_then(Value::as_array) {
        for (ordinal, entry) in after.iter().enumerate() {
            let Some(entry) = entry.as_str() else {
                continue;
            };
            let Some(full) = gate_source(entry, is_step) else {
                continue;
            };
            rows.push(row(
                ConsumerKind::Step,
                step,
                "after",
                ordinal as u32,
                RefKind::Gate,
                full,
            ));
        }
    }
    rows.sort_by(|a, b| (&a.slot, a.ordinal).cmp(&(&b.slot, b.ordinal)));
    rows
}

/// A plan output's one row: consumer `output`, slot `source`, ordinal 0, kind `output`.
pub fn output_references(name: &str, binding: &Map<String, Value>) -> Vec<ReferenceRow> {
    binding
        .get("source")
        .and_then(Value::as_str)
        .and_then(source)
        .map(|s| row(ConsumerKind::Output, name, "source", 0, RefKind::Output, s))
        .into_iter()
        .collect()
}

/// A `plan_edges` row: `target` depends on `source`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub source: String,
    pub target: String,
    /// `data` or `gate`.
    pub kind: &'static str,
    /// `''` unless the edge expands a `unit:` gate.
    pub via_unit: String,
}

/// The direct step dependencies of a declaration: (data sources, gate sources), each
/// distinct, current steps only.
fn direct(
    step: &str,
    declaration: &Map<String, Value>,
    is_step: &dyn Fn(&str) -> bool,
) -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
    let (mut data, mut gates, mut units) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    for reference in step_references(step, declaration, is_step) {
        match (reference.kind, reference.source_kind) {
            (RefKind::Binding, SourceKind::Step) if is_step(&reference.source_id) => {
                data.insert(reference.source_id);
            }
            (RefKind::Gate, SourceKind::Step) if is_step(&reference.source_id) => {
                gates.insert(reference.source_id);
            }
            (RefKind::Gate, SourceKind::Unit) => {
                units.insert(reference.source_id);
            }
            _ => {}
        }
    }
    (data, gates, units)
}

/// Each unit's exit steps: its steps tagged `exit`, else its sinks over the direct edges
/// between its own steps (`units::derive_units`).
pub fn unit_exits(steps: &[(String, Map<String, Value>)]) -> BTreeMap<String, Vec<String>> {
    let ids: BTreeSet<&str> = steps.iter().map(|(id, _)| id.as_str()).collect();
    let is_step = |id: &str| ids.contains(id);
    let mut members: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (id, declaration) in steps {
        members
            .entry(unit_of(id, declaration))
            .or_default()
            .push(id);
    }
    let by_id: BTreeMap<&str, &Map<String, Value>> =
        steps.iter().map(|(id, d)| (id.as_str(), d)).collect();
    members
        .into_iter()
        .map(|(unit, members)| {
            let tagged: Vec<String> = members
                .iter()
                .filter(|id| tags_of(by_id[*id]).iter().any(|t| t == "exit"))
                .map(|id| (*id).to_owned())
                .collect();
            if !tagged.is_empty() {
                return (unit, tagged);
            }
            let mut used = BTreeSet::new();
            for id in &members {
                let (data, gates, _) = direct(id, by_id[*id], &is_step);
                used.extend(
                    data.into_iter()
                        .chain(gates)
                        .filter(|d| members.contains(&d.as_str())),
                );
            }
            let sinks = members
                .iter()
                .filter(|id| !used.contains(**id))
                .map(|id| (*id).to_owned())
                .collect();
            (unit, sinks)
        })
        .collect()
}

/// Every `plan_edges` row of a plan: one `data` edge per distinct step source of a target's
/// bindings, one `gate` edge per distinct step source of its step and boolean gates, and one
/// `gate` edge from each exit of `u` (via `u`) per `unit:u` gate.
pub fn edges(steps: &[(String, Map<String, Value>)]) -> BTreeSet<Edge> {
    let ids: BTreeSet<&str> = steps.iter().map(|(id, _)| id.as_str()).collect();
    let is_step = |id: &str| ids.contains(id);
    let exits = unit_exits(steps);
    let mut edges = BTreeSet::new();
    for (target, declaration) in steps {
        let (data, gates, units) = direct(target, declaration, &is_step);
        let edge = |source: &str, kind, via: &str| Edge {
            source: source.to_owned(),
            target: target.clone(),
            kind,
            via_unit: via.to_owned(),
        };
        edges.extend(data.iter().map(|s| edge(s, "data", "")));
        edges.extend(gates.iter().map(|s| edge(s, "gate", "")));
        for unit in units {
            for exit in exits.get(&unit).into_iter().flatten() {
                edges.insert(edge(exit, "gate", &unit));
            }
        }
    }
    edges
}

/// A reference row as a sortable key.
pub type RefKey = (
    String,
    String,
    String,
    u32,
    String,
    String,
    String,
    String,
    String,
);
pub fn ref_key(row: &ReferenceRow) -> RefKey {
    fn word(value: serde_json::Result<Value>) -> String {
        match value {
            Ok(Value::String(word)) => word,
            other => panic!("not a word: {other:?}"),
        }
    }
    (
        word(serde_json::to_value(row.consumer_kind)),
        row.consumer_id.clone(),
        row.slot.clone(),
        row.ordinal,
        word(serde_json::to_value(row.kind)),
        word(serde_json::to_value(row.source_kind)),
        row.source_id.clone(),
        row.source_port.clone(),
        row.source_path.clone(),
    )
}

/// A project's rebuildable index rows, as stored or as derived.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Indexes {
    pub units: BTreeMap<String, String>,
    pub tags: BTreeSet<(String, String)>,
    pub references: BTreeSet<RefKey>,
    pub edges: BTreeSet<Edge>,
}
impl Indexes {
    /// What §2.5 derives from these declarations.
    pub fn derive(
        steps: &[(String, Map<String, Value>)],
        outputs: &[(String, Map<String, Value>)],
    ) -> Self {
        let ids: BTreeSet<&str> = steps.iter().map(|(id, _)| id.as_str()).collect();
        let is_step = |id: &str| ids.contains(id);
        let mut indexes = Self::default();
        for (id, declaration) in steps {
            indexes.units.insert(id.clone(), unit_of(id, declaration));
            for tag in tags_of(declaration) {
                indexes.tags.insert((id.clone(), tag));
            }
            indexes.references.extend(
                step_references(id, declaration, &is_step)
                    .iter()
                    .map(ref_key),
            );
        }
        for (name, binding) in outputs {
            indexes
                .references
                .extend(output_references(name, binding).iter().map(ref_key));
        }
        indexes.edges = edges(steps);
        indexes
    }
    /// What the store holds for `project`.
    pub fn stored(sql: &Connection, project: &str) -> rusqlite::Result<Self> {
        let mut indexes = Self::default();
        let mut q = sql.prepare("SELECT step_id, unit FROM steps WHERE project_id=?1")?;
        for row in q.query_map([project], |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (step, unit): (String, String) = row?;
            indexes.units.insert(step, unit);
        }
        let mut q = sql.prepare("SELECT step_id, tag FROM step_tags WHERE project_id=?1")?;
        for row in q.query_map([project], |r| Ok((r.get(0)?, r.get(1)?)))? {
            indexes.tags.insert(row?);
        }
        let mut q = sql.prepare(
            "SELECT consumer_kind, consumer_id, slot, ordinal, kind, source_kind, source_id,
                    source_port, source_path FROM plan_refs WHERE project_id=?1",
        )?;
        for row in q.query_map([project], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })? {
            indexes.references.insert(row?);
        }
        let mut q = sql.prepare(
            "SELECT source_step, target_step, kind, via_unit FROM plan_edges WHERE project_id=?1",
        )?;
        for row in q.query_map([project], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?, r.get(3)?))
        })? {
            let (source, target, kind, via_unit) = row?;
            indexes.edges.insert(Edge {
                source,
                target,
                kind: if kind == "data" { "data" } else { "gate" },
                via_unit,
            });
        }
        Ok(indexes)
    }
    /// Every difference, named.
    pub fn differences(&self, expected: &Self) -> Vec<String> {
        fn diff<T: Ord + std::fmt::Debug>(
            what: &str,
            stored: &BTreeSet<T>,
            expected: &BTreeSet<T>,
            out: &mut Vec<String>,
        ) {
            for extra in stored.difference(expected) {
                out.push(format!("{what}: stored but not derived: {extra:?}"));
            }
            for missing in expected.difference(stored) {
                out.push(format!("{what}: derived but not stored: {missing:?}"));
            }
        }
        let mut out = Vec::new();
        let units = |m: &BTreeMap<String, String>| -> BTreeSet<(String, String)> {
            m.iter().map(|(a, b)| (a.clone(), b.clone())).collect()
        };
        diff(
            "steps.unit",
            &units(&self.units),
            &units(&expected.units),
            &mut out,
        );
        diff("step_tags", &self.tags, &expected.tags, &mut out);
        diff(
            "plan_refs",
            &self.references,
            &expected.references,
            &mut out,
        );
        diff("plan_edges", &self.edges, &expected.edges, &mut out);
        out
    }
}

/// The authored rows of `project` as the store holds them, each collection in position order.
pub fn authored_rows(sql: &Connection, project: &str) -> rusqlite::Result<Rebuilt> {
    let mut rebuilt = Rebuilt::default();
    let order: Option<String> = sql
        .query_row(
            "SELECT root_order FROM plans WHERE project_id=?1",
            [project],
            |r| r.get(0),
        )
        .optional()?;
    rebuilt.root_order = order
        .map(|o| serde_json::from_str(&o).expect("root_order is JSON"))
        .unwrap_or_default();
    for (table, key, column, into) in [
        ("inputs", "name", "declaration", &mut rebuilt.inputs),
        ("plan_outputs", "name", "binding", &mut rebuilt.outputs),
        ("steps", "step_id", "declaration", &mut rebuilt.steps),
    ] {
        let mut q = sql.prepare(&format!(
            "SELECT {key}, position, {column} FROM {table} WHERE project_id=?1"
        ))?;
        for row in q.query_map([project], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (name, position, text) = row?;
            into.insert(
                name,
                (
                    position as u64,
                    serde_json::from_str(&text).expect("declaration is JSON"),
                ),
            );
        }
    }
    Ok(rebuilt)
}

/// Compare the stored index rows of `project` with what its stored declarations derive.
pub fn check_indexes(sql: &Connection, project: &str) -> Result<(), Vec<String>> {
    let rows = authored_rows(sql, project).map_err(|e| vec![e.to_string()])?;
    let (steps, outputs) = rows.declarations();
    let expected = Indexes::derive(&steps, &outputs);
    let stored = Indexes::stored(sql, project).map_err(|e| vec![e.to_string()])?;
    let differences = stored.differences(&expected);
    if differences.is_empty() {
        Ok(())
    } else {
        Err(differences)
    }
}

// ---- §5.3 and §7.2: rebuild from history, export ----------------------------------------

/// Declarations by id, in position order.
pub type Declarations = Vec<(String, Map<String, Value>)>;

/// A plan's authored rows: each collection by key, with its position and declaration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rebuilt {
    pub root_order: Vec<RootSection>,
    pub inputs: BTreeMap<String, (u64, Value)>,
    pub outputs: BTreeMap<String, (u64, Value)>,
    pub steps: BTreeMap<String, (u64, Value)>,
}
fn rank(change: &PlanChange) -> (u8, String, u64) {
    match change {
        PlanChange::HeaderPut { .. } => (0, String::new(), 0),
        PlanChange::InputDelete { name } => (1, name.clone(), 0),
        PlanChange::OutputDelete { name } => (2, name.clone(), 0),
        PlanChange::StepDelete { step } => (3, step.to_string(), 0),
        PlanChange::InputPut { position, .. } => (4, String::new(), *position),
        PlanChange::OutputPut { position, .. } => (5, String::new(), *position),
        PlanChange::StepPut { position, .. } => (6, String::new(), *position),
    }
}
fn identity(change: &PlanChange) -> (&'static str, String) {
    match change {
        PlanChange::HeaderPut { .. } => ("header", String::new()),
        PlanChange::InputDelete { name } | PlanChange::InputPut { name, .. } => {
            ("input", name.clone())
        }
        PlanChange::OutputDelete { name } | PlanChange::OutputPut { name, .. } => {
            ("output", name.clone())
        }
        PlanChange::StepDelete { step } | PlanChange::StepPut { step, .. } => {
            ("step", step.to_string())
        }
    }
}
/// §5.1's rule for one revision's changes: at most one per collection and key, listed
/// header, input, output and step deletes (by key), then input, output and step puts (by
/// position).
pub fn check_change_set(changes: &[PlanChange]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for change in changes {
        if !seen.insert(identity(change)) {
            return Err(format!("two changes for {:?}", identity(change)));
        }
    }
    for pair in changes.windows(2) {
        if rank(&pair[0]) > rank(&pair[1]) {
            return Err(format!("out of order: {:?} before {:?}", pair[0], pair[1]));
        }
    }
    Ok(())
}
impl Rebuilt {
    /// Apply one revision's changes as a set: deletes, then puts. Positions stay unique.
    pub fn apply(&mut self, changes: &[PlanChange]) -> Result<(), String> {
        check_change_set(changes)?;
        for change in changes {
            match change {
                PlanChange::HeaderPut { root_order } => self.root_order = root_order.clone(),
                PlanChange::InputDelete { name } => {
                    self.inputs.remove(name).ok_or(format!("no input {name}"))?;
                }
                PlanChange::OutputDelete { name } => {
                    self.outputs
                        .remove(name)
                        .ok_or(format!("no output {name}"))?;
                }
                PlanChange::StepDelete { step } => {
                    self.steps
                        .remove(step.as_str())
                        .ok_or(format!("no step {step}"))?;
                }
                PlanChange::InputPut {
                    name,
                    position,
                    declaration,
                } => {
                    self.inputs
                        .insert(name.clone(), (*position, declaration.as_value().clone()));
                }
                PlanChange::OutputPut {
                    name,
                    position,
                    binding,
                } => {
                    self.outputs
                        .insert(name.clone(), (*position, object(binding)));
                }
                PlanChange::StepPut {
                    step,
                    position,
                    declaration,
                } => {
                    self.steps
                        .insert(step.to_string(), (*position, object(declaration)));
                }
            }
        }
        for (what, rows) in [
            ("inputs", &self.inputs),
            ("outputs", &self.outputs),
            ("steps", &self.steps),
        ] {
            let positions: BTreeSet<u64> = rows.values().map(|(p, _)| *p).collect();
            if positions.len() != rows.len() {
                return Err(format!("{what}: two rows share a position"));
            }
        }
        Ok(())
    }
    fn ordered(rows: &BTreeMap<String, (u64, Value)>) -> Vec<(&String, &Value)> {
        let mut ordered: Vec<_> = rows.iter().map(|(k, (p, v))| (*p, k, v)).collect();
        ordered.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        ordered.into_iter().map(|(_, k, v)| (k, v)).collect()
    }
    /// The plan document: the sections in `root_order`, each in position order.
    pub fn export(&self) -> Map<String, Value> {
        let mut document = Map::new();
        for section in &self.root_order {
            let rows = match section {
                RootSection::Inputs => &self.inputs,
                RootSection::Outputs => &self.outputs,
                RootSection::Steps => &self.steps,
            };
            document.insert(
                section.as_str().to_owned(),
                Value::Object(
                    Self::ordered(rows)
                        .into_iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                ),
            );
        }
        document
    }
    /// The step and output declarations, in position order.
    pub fn declarations(&self) -> (Declarations, Declarations) {
        let objects = |rows: &BTreeMap<String, (u64, Value)>| {
            Self::ordered(rows)
                .into_iter()
                .map(|(k, v)| (k.clone(), v.as_object().cloned().unwrap_or_default()))
                .collect()
        };
        (objects(&self.steps), objects(&self.outputs))
    }
}

/// §10.5's equality: compact serializations, byte-equal.
pub fn compact(document: &Map<String, Value>) -> String {
    serde_json::to_string(document).expect("a JSON map serializes")
}

/// Rebuild `project`'s plan from `plan_edits.changes`, revision by revision: each
/// revision's export, starting at rev 1.
pub fn rebuild_history(sql: &Connection, project: &str) -> Result<Vec<(u64, String)>, String> {
    let mut q = sql
        .prepare("SELECT rev, changes FROM plan_edits WHERE project_id=?1 ORDER BY rev")
        .map_err(|e| e.to_string())?;
    let revisions: Vec<(i64, String)> = q
        .query_map([project], |r| Ok((r.get(0)?, r.get(1)?)))
        .and_then(Iterator::collect)
        .map_err(|e| e.to_string())?;
    let mut rebuilt = Rebuilt::default();
    let mut exports = Vec::new();
    for (index, (rev, changes)) in revisions.into_iter().enumerate() {
        if rev as usize != index + 1 {
            return Err(format!("history skips to rev {rev}"));
        }
        let changes: Vec<PlanChange> =
            serde_json::from_str(&changes).map_err(|e| format!("rev {rev}: {e}"))?;
        rebuilt
            .apply(&changes)
            .map_err(|e| format!("rev {rev}: {e}"))?;
        exports.push((rev as u64, compact(&rebuilt.export())));
    }
    Ok(exports)
}

fn object(map: &sluice_model::rpc::JsonMap) -> Value {
    Value::Object(
        map.0
            .iter()
            .map(|(k, v)| (k.clone(), v.as_value().clone()))
            .collect(),
    )
}

/// Whether `id` is a valid step id (for building `StepId`s in tests).
pub fn step_id(id: &str) -> StepId {
    StepId::new(id).expect("a step id")
}

// ---- §2.1: schema equivalence -----------------------------------------------------------

fn normalize(sql: &str) -> String {
    let without_comments: String = sql
        .lines()
        .map(|line| match line.find("--") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = String::new();
    for word in without_comments.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out.replace("( ", "(")
        .replace(" )", ")")
        .replace(" ,", ",")
        .replace(", ", ",")
}

/// The `CHECK (…)` clauses of a `CREATE TABLE`, normalized and sorted.
fn checks(sql: &str) -> Vec<String> {
    let text = normalize(sql);
    let upper = text.to_ascii_uppercase();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = upper[from..].find("CHECK") {
        let start = from + at;
        let Some(open) = text[start..].find('(').map(|o| start + o) else {
            break;
        };
        let mut depth = 0;
        let mut end = open;
        for (offset, ch) in text[open..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        found.push(text[open..=end].to_owned());
        from = end + 1;
    }
    found.sort();
    found
}

fn text(value: rusqlite::types::ValueRef<'_>) -> String {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => "NULL".to_owned(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => format!("{f:?}"),
        ValueRef::Text(t) => format!("{:?}", String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => format!(
            "x'{}'",
            b.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ),
    }
}

fn rows(sql: &Connection, query: &str, name: &str) -> rusqlite::Result<Vec<String>> {
    let mut q = sql.prepare(query)?;
    let columns = q.column_count();
    let out = q
        .query_map([name], |r| {
            (0..columns)
                .map(|i| r.get_ref(i).map(text))
                .collect::<rusqlite::Result<Vec<_>>>()
                .map(|v| v.join(" "))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

/// A database's schema as a sorted map from what each entry describes to its canonical text:
/// every table's columns (`pragma_table_xinfo`, in order: name, type, not-null, default,
/// primary key, hidden), its kind (strict, without rowid), foreign keys and `CHECK`s; every
/// index (`pragma_index_list` and `pragma_index_xinfo`, and a partial index's text); every
/// trigger and view by its normalized text.
pub fn schema_shape(sql: &Connection) -> rusqlite::Result<BTreeMap<String, String>> {
    let mut shape = BTreeMap::new();
    let mut q = sql.prepare(
        "SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
    )?;
    let entries: Vec<(String, String, String, String)> = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (kind, name, table, text) in entries {
        match kind.as_str() {
            "table" => {
                shape.insert(
                    format!("table {name} columns"),
                    rows(
                        sql,
                        "SELECT cid, name, type, \"notnull\", dflt_value, pk, hidden
                         FROM pragma_table_xinfo(?1) ORDER BY cid",
                        &name,
                    )?
                    .join("; "),
                );
                shape.insert(
                    format!("table {name} kind"),
                    rows(
                        sql,
                        "SELECT type, wr, strict FROM pragma_table_list WHERE name=?1",
                        &name,
                    )?
                    .join("; "),
                );
                let mut keys = rows(
                    sql,
                    "SELECT seq, \"table\", \"from\", \"to\", on_update, on_delete, \"match\"
                     FROM pragma_foreign_key_list(?1)",
                    &name,
                )?;
                keys.sort();
                shape.insert(format!("table {name} foreign keys"), keys.join("; "));
                shape.insert(format!("table {name} checks"), checks(&text).join("; "));
                let mut indexes = rows(
                    sql,
                    "SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1)",
                    &name,
                )?;
                indexes.sort();
                shape.insert(format!("table {name} indexes"), indexes.join("; "));
            }
            "index" => {
                let mut columns = rows(
                    sql,
                    "SELECT seqno, cid, name, \"desc\", coll, key FROM pragma_index_xinfo(?1)
                     ORDER BY seqno",
                    &name,
                )?;
                columns.insert(0, format!("on {table}"));
                if let Some(at) = text.to_ascii_uppercase().find(" WHERE ") {
                    columns.push(format!("where {}", normalize(&text[at..])));
                }
                shape.insert(format!("index {name}"), columns.join("; "));
            }
            "trigger" | "view" => {
                shape.insert(format!("{kind} {name}"), normalize(&text));
            }
            _ => {}
        }
    }
    Ok(shape)
}

/// Every entry two schemas disagree on, or that only one has.
pub fn schema_differences(
    left: &BTreeMap<String, String>,
    right: &BTreeMap<String, String>,
) -> Vec<String> {
    let keys: BTreeSet<&String> = left.keys().chain(right.keys()).collect();
    keys.into_iter()
        .filter_map(|key| match (left.get(key), right.get(key)) {
            (Some(a), Some(b)) if a == b => None,
            (a, b) => Some(format!("{key}:\n  left:  {a:?}\n  right: {b:?}")),
        })
        .collect()
}

// ---- logical content -----------------------------------------------------------------------

/// Everything a database holds, as text: its schema entries, every table's rows in rowid
/// order, and its `user_version` and `application_id`. Equal text means nothing changed.
pub fn logical_dump(sql: &Connection) -> rusqlite::Result<String> {
    let mut out = String::new();
    for pragma in ["user_version", "application_id"] {
        let value: i64 = sql.pragma_query_value(None, pragma, |r| r.get(0))?;
        out.push_str(&format!("PRAGMA {pragma}={value};\n"));
    }
    let mut q =
        sql.prepare("SELECT type, name, coalesce(sql, '') FROM sqlite_schema ORDER BY type, name")?;
    let entries: Vec<(String, String, String)> = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (kind, name, text) in &entries {
        out.push_str(&format!("{kind} {name}: {text}\n"));
    }
    for (kind, name, _) in &entries {
        if kind != "table" {
            continue;
        }
        let mut q = sql.prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid"))?;
        let columns = q.column_count();
        let lines = q
            .query_map([], |r| {
                (0..columns)
                    .map(|i| r.get_ref(i).map(text))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for line in lines {
            out.push_str(&format!("{name}: {}\n", line.join(", ")));
        }
    }
    Ok(out)
}

/// Load a dump (`tools/cutover-rehearsal`'s SQL text) into a new database file.
pub fn load_dump(path: &std::path::Path, dump: &str) -> rusqlite::Result<()> {
    let sql = Connection::open(path)?;
    sql.execute_batch(dump)
}

/// `EXPLAIN QUERY PLAN`'s detail lines.
pub fn query_plan(sql: &Connection, query: &str) -> rusqlite::Result<Vec<String>> {
    let mut q = sql.prepare(&format!("EXPLAIN QUERY PLAN {query}"))?;
    q.query_map([], |r| r.get::<_, String>(3))?.collect()
}
