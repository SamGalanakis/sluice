//! Compiled plan semantics. This module never reads files or invokes a function.
//!
//! The plan's truth is its rows (`plan_rows::PlanRows`): each input, output and step as
//! written, at its position. `compile_rows` compiles them whole (a cold cache, `verify`); an
//! edit compiles only what it changes (`prepare_plan_edit`), on a candidate that shares
//! structure with its certified base (`persistent`), so an edit costs what it changes, not
//! the plan's size. The compiled `Plan` keeps today's query API (`inputs`, `outputs`, `steps`,
//! `units`, `dependencies`, `topological_order`, `reference_type`) and, beside it, each row as
//! written and the reverse indexes incremental validation reads (who reads a name, who gates
//! on a unit, who needs a resource, who carries a tag).

mod compile;
mod index;
mod ops;
mod prepare;

pub use crate::plan_index::{output_references, step_index};
pub use crate::plan_rows::CertifiedPlan;
pub use compile::{compile_rows, full_compiles, reset_counters};
pub use index::step_edges;
pub use prepare::{EditBase, PrepareOptions, preparation_reads, prepare_plan_edit};

pub(crate) use compile::{Change, Delta, compile_delta};
pub(crate) use index::{decl_dependencies, entry_error, unit_of};
pub(crate) use ops::{Applied, apply_ops};

use crate::{
    gates::{Gate, Reference, ValueRef},
    hash::{EffectiveInput, InputsHash},
    ids::{StepId, UnitName},
    persistent::{Ordered, Tree},
    plan_rows::RootSection,
    rpc::{JsonMap, JsonValue},
    types::{PathError, Type, check_value_at, fits, navigate},
    units::Unit,
};
use indexmap::{IndexMap, IndexSet};
use serde_json::Value;
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub ty: Type,
    pub doc: Option<String>,
}

/// Registry-owned signatures are passed by value; no registry dependency enters the model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FnSignature {
    pub inputs: IndexMap<String, Type>,
    pub outputs: IndexMap<String, Type>,
    pub submits: IndexMap<String, Declaration>,
    pub open: bool,
}
pub trait SignatureProvider {
    fn signature(&self, name: &str) -> Option<FnSignature>;
}
impl SignatureProvider for IndexMap<String, FnSignature> {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        self.get(name).cloned()
    }
}

#[derive(Debug, Clone)]
pub enum Binding {
    Default(JsonValue),
    Source(ValueRef),
    Sources(Vec<ValueRef>),
    File(String),
}
impl PartialEq for Binding {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Default(left), Self::Default(right)) => {
                crate::hash::data_equal(left.as_value(), right.as_value())
                    .expect("strict binding values")
            }
            (Self::Source(left), Self::Source(right)) => left == right,
            (Self::Sources(left), Self::Sources(right)) => left == right,
            (Self::File(left), Self::File(right)) => left == right,
            _ => false,
        }
    }
}
impl Binding {
    pub fn references(&self) -> &[ValueRef] {
        match self {
            Self::Source(reference) => std::slice::from_ref(reference),
            Self::Sources(references) => references,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Pause {
    #[default]
    No,
    Yes,
    Reason(String),
}
impl Pause {
    pub fn is_paused(&self) -> bool {
        !matches!(self, Self::No)
    }
    pub fn waiting_reason(&self) -> Option<String> {
        match self {
            Self::No => None,
            Self::Yes => Some("paused".into()),
            Self::Reason(reason) => Some(format!("paused: {reason}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub id: StepId,
    pub run: String,
    pub signature: FnSignature,
    pub bindings: IndexMap<String, Binding>,
    pub extra_inputs: IndexMap<String, Type>,
    pub declared_outputs: IndexMap<String, Declaration>,
    pub scatter: Option<String>,
    pub doc: Option<String>,
    pub paused: Pause,
    pub after: Vec<Gate>,
    pub tags: Vec<String>,
    pub needs: IndexMap<String, u64>,
    pub priority: i64,
    /// The declaration exactly as written (the `steps` row's `declaration`).
    pub declaration: JsonMap,
}
impl Step {
    pub fn is_external(&self) -> bool {
        self.run == "core.external"
    }
    pub fn unit_name(&self) -> UnitName {
        self.tags
            .iter()
            .find_map(|tag| tag.strip_prefix("unit:"))
            .unwrap_or(self.id.as_str())
            .parse()
            .expect("validated unit name")
    }
    pub fn output_type(&self, name: &str) -> Option<Type> {
        let ty = self
            .signature
            .outputs
            .get(name)
            .or_else(|| self.declared_outputs.get(name).map(|decl| &decl.ty))?
            .clone();
        Some(if self.scatter.is_some() {
            Type::List(Box::new(ty))
        } else {
            ty
        })
    }
    pub fn data_dependencies(&self) -> Vec<StepId> {
        self.bindings
            .values()
            .flat_map(Binding::references)
            .filter_map(|reference| reference.parts().ok()?.step)
            .collect::<IndexSet<_>>()
            .into_iter()
            .collect()
    }
    /// Whether `self` and `other` mean the same work, `paused` and `tags` aside: what a
    /// running step may change (and nothing else).
    pub fn same_work(&self, other: &Self) -> bool {
        self.id == other.id
            && self.run == other.run
            && self.signature == other.signature
            && self.bindings == other.bindings
            && self.extra_inputs == other.extra_inputs
            && self.declared_outputs == other.declared_outputs
            && self.scatter == other.scatter
            && self.doc == other.doc
            && self.after == other.after
            && self.needs == other.needs
            && self.priority == other.priority
    }
}

/// Who reads a name: a step (a binding or a gate) or a plan output.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Consumer {
    Step(StepId),
    Output(String),
}

/// A compiled, validated plan: only `compile_rows` and an edit's preparation make one. Its
/// maps are persistent: a candidate built from a base shares every row and subtree the edit
/// leaves alone. Queries keep plan order (each collection by position).
#[derive(Clone)]
pub struct Plan {
    root_order: Vec<RootSection>,
    inputs: Ordered<String, Declaration>,
    written_inputs: Tree<String, JsonValue>,
    outputs: Ordered<String, ValueRef>,
    written_outputs: Tree<String, JsonMap>,
    steps: Ordered<StepId, Step>,
    units: Ordered<UnitName, Unit>,
    dependencies: Tree<StepId, Arc<[StepId]>>,
    /// Name (a step id or a plan input) → the steps and outputs whose declarations read it.
    readers: Tree<String, Tree<Consumer, ()>>,
    /// Unit → the steps gating on it (`unit:<u>`).
    unit_readers: Tree<UnitName, Tree<StepId, ()>>,
    /// Resource → the steps that need it.
    needing: Tree<String, Tree<StepId, ()>>,
    /// Tag → the steps that carry it.
    tagged: Tree<String, Tree<StepId, ()>>,
    /// The whole topological order, worked out on first use.
    order: OnceLock<Arc<[StepId]>>,
}
impl PartialEq for Plan {
    fn eq(&self, other: &Self) -> bool {
        self.root_order == other.root_order
            && self.inputs == other.inputs
            && self.written_inputs == other.written_inputs
            && self.outputs == other.outputs
            && self.written_outputs == other.written_outputs
            && self.steps == other.steps
            && self.units == other.units
            && self.dependencies == other.dependencies
    }
}
impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field("root_order", &self.root_order)
            .field("inputs", &self.inputs)
            .field("outputs", &self.outputs)
            .field("steps", &self.steps.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Plan {
    /// No sections, no rows.
    pub(crate) fn empty() -> Self {
        Self {
            root_order: vec![],
            inputs: Ordered::new(),
            written_inputs: Tree::new(),
            outputs: Ordered::new(),
            written_outputs: Tree::new(),
            steps: Ordered::new(),
            units: Ordered::new(),
            dependencies: Tree::new(),
            readers: Tree::new(),
            unit_readers: Tree::new(),
            needing: Tree::new(),
            tagged: Tree::new(),
            order: OnceLock::new(),
        }
    }
    /// The present root sections, in document order.
    pub fn root_order(&self) -> &[RootSection] {
        &self.root_order
    }
    pub fn inputs(&self) -> &Ordered<String, Declaration> {
        &self.inputs
    }
    pub fn outputs(&self) -> &Ordered<String, ValueRef> {
        &self.outputs
    }
    pub fn steps(&self) -> &Ordered<StepId, Step> {
        &self.steps
    }
    pub fn units(&self) -> &Ordered<UnitName, Unit> {
        &self.units
    }
    pub fn dependencies(&self, id: &StepId) -> &[StepId] {
        self.dependencies
            .get(id)
            .map(|dependencies| &**dependencies)
            .unwrap_or_default()
    }
    /// Every step after the steps it depends on: a depth-first post-order from each step in
    /// plan order. Worked out once per compiled plan, on first use.
    pub fn topological_order(&self) -> &[StepId] {
        self.order.get_or_init(|| {
            self.cycle_free_order()
                .expect("a compiled plan has no dependency cycle")
                .into()
        })
    }
    /// A step's position.
    pub fn position(&self, id: &StepId) -> Option<u64> {
        self.steps.position(id)
    }
    /// A plan input's declaration exactly as written, and its position.
    pub fn input_row(&self, name: &str) -> Option<(u64, &JsonValue)> {
        Some((self.inputs.position(name)?, self.written_inputs.get(name)?))
    }
    /// A plan output's binding exactly as written, and its position.
    pub fn output_row(&self, name: &str) -> Option<(u64, &JsonMap)> {
        Some((
            self.outputs.position(name)?,
            self.written_outputs.get(name)?,
        ))
    }
    /// The steps carrying `tag`, in no particular order.
    pub fn tagged(&self, tag: &str) -> impl Iterator<Item = &StepId> {
        self.tagged.get(tag).into_iter().flat_map(Tree::keys)
    }
    /// The steps that need `resource`, in no particular order.
    pub fn needing(&self, resource: &str) -> impl Iterator<Item = &StepId> {
        self.needing.get(resource).into_iter().flat_map(Tree::keys)
    }
    /// The steps and plan outputs whose declarations read `name` (a step or a plan input).
    pub(crate) fn readers(&self, name: &str) -> impl Iterator<Item = &Consumer> {
        self.readers.get(name).into_iter().flat_map(Tree::keys)
    }
    /// The steps gating on `unit:<unit>`.
    pub(crate) fn unit_readers(&self, unit: &UnitName) -> impl Iterator<Item = &StepId> {
        self.unit_readers.get(unit).into_iter().flat_map(Tree::keys)
    }
    /// The steps that depend on `id` (`dependencies` read backwards): its readers, and the
    /// steps gating on its unit when it is one of the unit's exits.
    pub fn dependents(&self, id: &StepId) -> IndexSet<StepId> {
        let mut out: IndexSet<StepId> = self
            .readers(id.as_str())
            .filter_map(|consumer| match consumer {
                Consumer::Step(step) => Some(step.clone()),
                Consumer::Output(_) => None,
            })
            .filter(|step| self.dependencies(step).contains(id))
            .collect();
        if let Some(step) = self.steps.get(id) {
            let unit = step.unit_name();
            if self.units.get(&unit).is_some_and(|u| u.exits.contains(id)) {
                out.extend(
                    self.unit_readers(&unit)
                        .filter(|reader| self.dependencies(reader).contains(id))
                        .cloned(),
                );
            }
        }
        out
    }
    /// The topological order of every step, or the first dependency cycle found.
    pub(crate) fn cycle_free_order(&self) -> Result<Vec<StepId>, Vec<StepId>> {
        let mut marks = IndexMap::<&StepId, u8>::new();
        let mut order = vec![];
        for root in self.steps.keys() {
            if marks.contains_key(root) {
                continue;
            }
            let mut stack: Vec<(&StepId, usize)> = vec![(root, 0)];
            marks.insert(root, 1);
            while let Some((id, index)) = stack.last_mut() {
                let dependencies = self.dependencies(id);
                if *index == dependencies.len() {
                    let id = *id;
                    stack.pop();
                    marks.insert(id, 2);
                    order.push(id.clone());
                    continue;
                }
                let next = &dependencies[*index];
                *index += 1;
                let Some((next, _)) = self.steps.get(next).map(|step| (&step.id, ())) else {
                    continue;
                };
                match marks.get(next) {
                    Some(1) => {
                        let first = stack
                            .iter()
                            .position(|(id, _)| *id == next)
                            .expect("ancestor");
                        let mut cycle: Vec<_> =
                            stack[first..].iter().map(|(id, _)| (*id).clone()).collect();
                        cycle.push(next.clone());
                        return Err(cycle);
                    }
                    Some(_) => {}
                    None => {
                        marks.insert(next, 1);
                        stack.push((next, 0));
                    }
                }
            }
        }
        Ok(order)
    }
    pub fn reference_type(&self, reference: &ValueRef) -> Result<Type, String> {
        let Reference { step, name, fields } = reference.parts()?;
        let base = match step {
            None => self
                .inputs
                .get(&name)
                .map(|decl| decl.ty.clone())
                .ok_or_else(|| format!("unknown plan input {name}"))?,
            Some(id) => {
                let step = self
                    .steps
                    .get(&id)
                    .ok_or_else(|| format!("unknown step {id}"))?;
                step.output_type(&name)
                    .ok_or_else(|| format!("step {id} (fn {}) has no output {name}", step.run))?
            }
        };
        navigate(&base, &fields.join("."))
            .map_err(|error| format!("{reference}: {}", error.message))
    }
    pub fn validate_input_values(&self, inputs: &JsonMap) -> Result<(), Vec<PathError>> {
        let mut errors = Vec::new();
        for (name, value) in &inputs.0 {
            let path = format!("inputs.{name}");
            match self.inputs.get(name) {
                Some(decl) => {
                    if let Err(found) = check_value_at(&decl.ty, value.as_value(), &path) {
                        errors.extend(found);
                    }
                }
                None => errors.push(diagnostic(&path, "unknown plan input")),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

pub(crate) fn diagnostic(path: &str, message: impl Into<String>) -> PathError {
    PathError {
        path: path.into(),
        message: message.into(),
    }
}
fn object<'a>(
    value: &'a Value,
    path: &str,
    errors: &mut Vec<PathError>,
) -> Option<&'a serde_json::Map<String, Value>> {
    value.as_object().or_else(|| {
        errors.push(diagnostic(path, "expected an object"));
        None
    })
}
fn closed(
    raw: &serde_json::Map<String, Value>,
    keys: &[&str],
    path: &str,
    errors: &mut Vec<PathError>,
) {
    for key in raw.keys().filter(|key| !keys.contains(&key.as_str())) {
        let path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        errors.push(diagnostic(
            &path,
            if key == "rev" {
                "maintained by the store"
            } else {
                "unknown key"
            },
        ));
    }
}
pub(crate) fn valid_id(name: &str, path: &str, errors: &mut Vec<PathError>) -> bool {
    if StepId::new(name).is_ok() {
        true
    } else {
        errors.push(diagnostic(path, "ids match ^[a-z0-9][a-z0-9_-]*$"));
        false
    }
}
fn string(value: &Value, path: &str, errors: &mut Vec<PathError>) -> Option<String> {
    value.as_str().map(str::to_owned).or_else(|| {
        errors.push(diagnostic(path, "expected a string"));
        None
    })
}
pub(crate) fn declaration(
    raw: &Value,
    path: &str,
    errors: &mut Vec<PathError>,
) -> Option<Declaration> {
    let (form, doc) = if let Some(map) = raw
        .as_object()
        .filter(|map| map.contains_key("doc") || map.get("type").is_some_and(|ty| !ty.is_string()))
    {
        closed(map, &["type", "doc"], path, errors);
        let doc = map
            .get("doc")
            .and_then(|value| string(value, &format!("{path}.doc"), errors));
        let Some(form) = map.get("type") else {
            errors.push(diagnostic(&format!("{path}.type"), "required"));
            return None;
        };
        (form, doc)
    } else if let Some(map) = raw
        .as_object()
        .filter(|map| map.len() == 1 && map.contains_key("type"))
    {
        (&map["type"], None)
    } else {
        (raw, None)
    };
    match Type::parse(form) {
        Ok(ty) => Some(Declaration { ty, doc }),
        Err(error) => {
            errors.push(diagnostic(
                &error.path.replacen("type", path, 1),
                error.message,
            ));
            None
        }
    }
}
fn declarations(
    raw: &Value,
    path: &str,
    errors: &mut Vec<PathError>,
) -> IndexMap<String, Declaration> {
    let mut result = IndexMap::new();
    if let Some(map) = object(raw, path, errors) {
        for (name, value) in map {
            let path = format!("{path}.{name}");
            if valid_id(name, &path, errors)
                && let Some(decl) = declaration(value, &path, errors)
            {
                result.insert(name.clone(), decl);
            }
        }
    }
    result
}
fn reference(raw: &Value, path: &str, errors: &mut Vec<PathError>) -> Option<ValueRef> {
    let text = string(raw, path, errors)?;
    let reference = ValueRef(text);
    match reference.parts() {
        Ok(_) => Some(reference),
        Err(error) => {
            errors.push(diagnostic(path, error));
            None
        }
    }
}
pub(crate) fn binding(raw: &Value, path: &str, errors: &mut Vec<PathError>) -> Option<Binding> {
    let Some(map) = raw.as_object().filter(|map| {
        map.len() == 1
            && map
                .keys()
                .all(|key| ["default", "source", "file"].contains(&key.as_str()))
    }) else {
        errors.push(diagnostic(
            path,
            "expected {\"default\": ...}, {\"source\": ...} or {\"file\": \"/abs/path\"}",
        ));
        return None;
    };
    if let Some(value) = map.get("default") {
        return match JsonValue::try_from(value.clone()) {
            Ok(value) => Some(Binding::Default(value)),
            Err(error) => {
                errors.push(diagnostic(path, error.to_string()));
                None
            }
        };
    }
    if let Some(value) = map.get("file") {
        let file = string(value, &format!("{path}.file"), errors)?;
        if !file.starts_with('/') {
            errors.push(diagnostic(
                &format!("{path}.file"),
                "expected an absolute path (a string)",
            ));
            return None;
        }
        return Some(Binding::File(file));
    }
    let value = &map["source"];
    if let Some(values) = value.as_array() {
        let references: Vec<_> = values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| {
                reference(value, &format!("{path}.source[{index}]"), errors)
            })
            .collect();
        if references.len() == values.len() {
            Some(Binding::Sources(references))
        } else {
            None
        }
    } else {
        reference(value, &format!("{path}.source"), errors).map(Binding::Source)
    }
}
fn string_list(raw: &Value, path: &str, errors: &mut Vec<PathError>) -> Vec<String> {
    let Some(values) = raw.as_array() else {
        errors.push(diagnostic(path, "expected an array of strings"));
        return vec![];
    };
    values
        .iter()
        .enumerate()
        .filter_map(|(i, value)| string(value, &format!("{path}[{i}]"), errors))
        .collect::<IndexSet<_>>()
        .into_iter()
        .collect()
}
pub(crate) fn map_value(map: &JsonMap) -> Value {
    Value::Object(
        map.0
            .iter()
            .map(|(key, value)| (key.clone(), value.as_value().clone()))
            .collect(),
    )
}

/// A step's declaration on its own: its keys, fn, bindings' shapes, declared outputs, scatter,
/// pause, tags, needs and priority. Bindings' sources and gates are checked against the rest of
/// the plan afterwards (`check_bindings`, `compile_gates`). `None` when it cannot be a step.
pub(crate) fn parse_step(
    id: StepId,
    declaration: &JsonMap,
    signatures: &impl SignatureProvider,
    errors: &mut Vec<PathError>,
) -> Option<Step> {
    let path = format!("steps.{id}");
    let value = map_value(declaration);
    let raw = object(&value, &path, errors)?;
    closed(
        raw,
        &[
            "run", "in", "scatter", "doc", "outputs", "paused", "after", "tags", "needs",
            "priority",
        ],
        &path,
        errors,
    );
    let run = raw
        .get("run")
        .and_then(|value| string(value, &format!("{path}.run"), errors));
    let signature = run.as_deref().and_then(|name| signatures.signature(name));
    if signature.is_none() {
        errors.push(diagnostic(
            &format!("{path}.run"),
            format!("unknown fn {}", raw.get("run").unwrap_or(&Value::Null)),
        ));
    }
    let mut bindings = IndexMap::new();
    if let Some(value) = raw.get("in")
        && let Some(map) = object(value, &format!("{path}.in"), errors)
    {
        for (name, value) in map {
            if let Some(binding) = binding(value, &format!("{path}.in.{name}"), errors) {
                bindings.insert(name.clone(), binding);
            }
        }
    }
    let declared = raw
        .get("outputs")
        .map(|value| declarations(value, &format!("{path}.outputs"), errors))
        .unwrap_or_default();
    let doc = raw
        .get("doc")
        .and_then(|value| string(value, &format!("{path}.doc"), errors));
    let scatter = raw
        .get("scatter")
        .and_then(|value| string(value, &format!("{path}.scatter"), errors));
    let paused = match raw.get("paused") {
        None | Some(Value::Bool(false)) => Pause::No,
        Some(Value::Bool(true)) => Pause::Yes,
        Some(Value::String(reason)) if !reason.trim().is_empty() => Pause::Reason(reason.clone()),
        _ => {
            errors.push(diagnostic(
                &format!("{path}.paused"),
                "expected true, false or the reason (a string)",
            ));
            Pause::No
        }
    };
    let tags = raw
        .get("tags")
        .map(|value| string_list(value, &format!("{path}.tags"), errors))
        .unwrap_or_default();
    for tag in &tags {
        if tag.split(':').count() > 2 || tag.split(':').any(|part| StepId::new(part).is_err()) {
            errors.push(diagnostic(
                &format!("{path}.tags"),
                "expected tags matching ^([a-z0-9][a-z0-9_-]*:)?[a-z0-9][a-z0-9_-]*$",
            ));
        }
    }
    if tags.iter().filter(|tag| tag.starts_with("unit:")).count() > 1 {
        errors.push(diagnostic(
            &format!("{path}.tags"),
            "a step carries at most one unit: tag",
        ));
    }
    let mut needs = IndexMap::new();
    if let Some(value) = raw.get("needs")
        && let Some(map) = object(value, &format!("{path}.needs"), errors)
    {
        for (name, value) in map {
            let path = format!("{path}.needs.{name}");
            valid_id(name, &path, errors);
            match value.as_u64() {
                Some(amount) => {
                    needs.insert(name.clone(), amount);
                }
                None => errors.push(diagnostic(&path, "expected an integer >= 0")),
            }
        }
    }
    let priority = match raw.get("priority") {
        None => 0,
        Some(value) => value.as_i64().unwrap_or_else(|| {
            errors.push(diagnostic(
                &format!("{path}.priority"),
                "expected an integer (higher is admitted first)",
            ));
            0
        }),
    };
    // Gates compile after every step, input and output signature is known.
    if let Some(value) = raw.get("after") {
        string_list(value, &format!("{path}.after"), errors);
    }
    let signature = signature?;
    for (name, ty) in &signature.inputs {
        if !matches!(ty, Type::Optional(_))
            && !raw
                .get("in")
                .and_then(Value::as_object)
                .is_some_and(|inputs| inputs.contains_key(name))
        {
            errors.push(diagnostic(
                &format!("{path}.in.{name}"),
                "required input is not bound",
            ));
        }
    }
    for name in bindings
        .keys()
        .filter(|name| !signature.inputs.contains_key(*name))
    {
        let path = format!("{path}.in.{name}");
        if signature.open {
            valid_id(name, &path, errors);
        } else {
            errors.push(diagnostic(
                &path,
                format!(
                    "fn {} has no input {name} (only an open fn takes extra inputs)",
                    run.as_deref().unwrap_or_default()
                ),
            ));
        }
    }
    if raw.contains_key("outputs") && !signature.open {
        errors.push(diagnostic(
            &format!("{path}.outputs"),
            format!(
                "fn {} is not open; only a step running an open fn declares outputs",
                run.as_deref().unwrap_or_default()
            ),
        ));
    }
    let mut declared_outputs = signature.submits.clone();
    for (name, decl) in declared {
        if signature.outputs.contains_key(&name) || signature.submits.contains_key(&name) {
            errors.push(diagnostic(
                &format!("{path}.outputs.{name}"),
                format!(
                    "fn {} already has an output {name}",
                    run.as_deref().unwrap_or_default()
                ),
            ));
        } else {
            declared_outputs.insert(name, decl);
        }
    }
    if let Some(name) = &scatter {
        if run.as_deref() == Some("core.external") {
            errors.push(diagnostic(
                &format!("{path}.scatter"),
                "core.external is one piece of outside work; it does not scatter",
            ));
        }
        if !bindings.contains_key(name) {
            errors.push(diagnostic(
                &format!("{path}.scatter"),
                format!("{name:?} is not a bound input of the step"),
            ));
        }
    }
    Some(Step {
        id,
        run: run?,
        signature,
        bindings,
        extra_inputs: IndexMap::new(),
        declared_outputs,
        scatter,
        doc,
        paused,
        after: vec![],
        tags,
        needs,
        priority,
        declaration: declaration.clone(),
    })
}

/// A plan input's declaration (`inputs.<name>`): its name, then its type.
pub(crate) fn parse_input(name: &str, raw: &JsonValue) -> (Option<Declaration>, Vec<PathError>) {
    let mut errors = vec![];
    let path = format!("inputs.{name}");
    let declaration = valid_id(name, &path, &mut errors)
        .then(|| declaration(raw.as_value(), &path, &mut errors))
        .flatten();
    (declaration, errors)
}

/// A plan output (`outputs.<name>`): its name, its shape (`{"source": "<ref>"}`) and its
/// source against `plan`. The source, when the binding has one.
pub(crate) fn check_output(
    plan: &Plan,
    name: &str,
    binding_map: &JsonMap,
) -> (Option<ValueRef>, Vec<PathError>) {
    let mut errors = vec![];
    let path = format!("outputs.{name}");
    valid_id(name, &path, &mut errors);
    let source = match binding(&map_value(binding_map), &path, &mut errors) {
        Some(Binding::Source(reference)) => {
            if let Err(error) = plan.reference_type(&reference) {
                errors.push(diagnostic(&path, error));
            }
            Some(reference)
        }
        _ => {
            errors.push(diagnostic(&path, "expected {\"source\": \"<ref>\"}"));
            None
        }
    };
    (source, errors)
}

/// A step's bindings against the plan: each source's type against its input's (item-wise for
/// the scatter input, element-wise for a list source), and an open fn's extra inputs typed.
pub(crate) fn check_bindings(plan: &Plan, step: &Step) -> (IndexMap<String, Type>, Vec<PathError>) {
    let mut errors = vec![];
    let mut extras = IndexMap::new();
    let id = &step.id;
    for (name, binding) in &step.bindings {
        let path = format!("steps.{id}.in.{name}");
        if let Some(target) = step.signature.inputs.get(name) {
            let target = if step.scatter.as_ref() == Some(name) {
                Type::List(Box::new(target.clone()))
            } else {
                target.clone()
            };
            check_binding(binding, &target, plan, &path, &mut errors);
        } else if step.signature.open {
            let mut ty = binding_type(binding, plan, &path, &mut errors);
            if step.scatter.as_ref() == Some(name) {
                let inner = match &ty {
                    Type::Optional(inner) => inner.as_ref(),
                    _ => &ty,
                };
                if let Binding::Default(value) = binding
                    && !value.as_value().is_array()
                {
                    errors.push(diagnostic(&path, "the scatter input needs an array"));
                }
                ty = match inner {
                    Type::List(item) => item.as_ref().clone(),
                    Type::Any => Type::Any,
                    _ => {
                        errors.push(diagnostic(
                            &path,
                            format!("the scatter input needs an array, not {ty}"),
                        ));
                        Type::Any
                    }
                };
            }
            extras.insert(name.clone(), ty);
        }
    }
    (extras, errors)
}

/// A step's gate entries (its declaration's `after`), compiled against the plan, each once.
pub(crate) fn compile_gates(plan: &Plan, step: &Step) -> (Vec<Gate>, Vec<PathError>) {
    let mut errors = vec![];
    let mut gates = vec![];
    if let Some(entries) = step
        .declaration
        .0
        .get("after")
        .and_then(|after| after.as_value().as_array())
    {
        for (index, entry) in entries.iter().enumerate() {
            if let Some(text) = entry.as_str() {
                match Gate::compile(text, plan) {
                    Ok(gate) => {
                        if !gates.contains(&gate) {
                            gates.push(gate);
                        }
                    }
                    Err(error) => errors.push(diagnostic(
                        &format!("steps.{}.after[{index}]", step.id),
                        error,
                    )),
                }
            }
        }
    }
    (gates, errors)
}

fn binding_type(binding: &Binding, plan: &Plan, path: &str, errors: &mut Vec<PathError>) -> Type {
    match binding {
        Binding::Default(_) => Type::Any,
        Binding::File(_) => Type::String,
        Binding::Source(reference) => plan.reference_type(reference).unwrap_or_else(|error| {
            errors.push(diagnostic(path, error));
            Type::Any
        }),
        Binding::Sources(references) => {
            let types: Vec<_> = references
                .iter()
                .enumerate()
                .map(|(index, reference)| {
                    plan.reference_type(reference).unwrap_or_else(|error| {
                        errors.push(diagnostic(&format!("{path}.source[{index}]"), error));
                        Type::Any
                    })
                })
                .collect();
            Type::List(Box::new(
                types
                    .first()
                    .filter(|first| types.iter().all(|ty| ty == *first))
                    .cloned()
                    .unwrap_or(Type::Any),
            ))
        }
    }
}
fn check_binding(
    binding: &Binding,
    target: &Type,
    plan: &Plan,
    path: &str,
    errors: &mut Vec<PathError>,
) {
    match binding {
        Binding::Default(value) => {
            if let Err(found) = check_value_at(target, value.as_value(), path) {
                errors.extend(found);
            }
        }
        Binding::File(_) => {
            if !fits(&Type::String, target) {
                errors.push(diagnostic(
                    path,
                    format!("a file binding is a string, which does not fit {target}"),
                ));
            }
        }
        Binding::Source(reference) => check_reference(reference, target, plan, path, errors),
        Binding::Sources(references) => {
            let target = match target {
                Type::Optional(inner) => inner.as_ref(),
                _ => target,
            };
            let item = match target {
                Type::List(item) => item.as_ref(),
                Type::Any => &Type::Any,
                _ => {
                    errors.push(diagnostic(
                        path,
                        format!("a list source needs an array or Any input, not {target}"),
                    ));
                    return;
                }
            };
            for (index, reference) in references.iter().enumerate() {
                check_reference(
                    reference,
                    item,
                    plan,
                    &format!("{path}.source[{index}]"),
                    errors,
                );
            }
        }
    }
}
fn check_reference(
    reference: &ValueRef,
    target: &Type,
    plan: &Plan,
    path: &str,
    errors: &mut Vec<PathError>,
) {
    match plan.reference_type(reference) {
        Err(error) => errors.push(diagnostic(path, error)),
        Ok(source) if !fits(&source, target) => errors.push(diagnostic(
            path,
            format!("{reference} is {source}, which does not fit {target}"),
        )),
        _ => {}
    }
}

/// A step's dependencies: the steps its bindings read (each once), then each gate's (a step
/// gate's step, a boolean gate's step, a unit gate's exits).
pub(crate) fn expanded_dependencies(plan: &Plan, step: &Step) -> Vec<StepId> {
    let mut dependencies: IndexSet<_> = step.data_dependencies().into_iter().collect();
    for gate in &step.after {
        match gate {
            Gate::Step { id, .. } => {
                dependencies.insert(id.clone());
            }
            Gate::Bool { reference, .. } => {
                if let Ok(Reference { step: Some(id), .. }) = reference.parts() {
                    dependencies.insert(id);
                }
            }
            Gate::Unit { name, .. } => {
                if let Some(unit) = plan.units.get(name) {
                    dependencies.extend(unit.exits.iter().cloned());
                }
            }
        }
    }
    dependencies.into_iter().collect()
}
/// Iterative DFS: bounded by graph size, with a deterministic cycle witness.
pub fn topological_order(
    graph: &IndexMap<StepId, Vec<StepId>>,
) -> Result<Vec<StepId>, Vec<StepId>> {
    let mut marks = IndexMap::<StepId, u8>::new();
    let mut order = vec![];
    for root in graph.keys() {
        if marks.contains_key(root) {
            continue;
        }
        let mut stack = vec![(root.clone(), 0)];
        marks.insert(root.clone(), 1);
        while let Some((id, index)) = stack.last_mut() {
            let dependencies = &graph[&*id];
            if *index == dependencies.len() {
                let id = id.clone();
                stack.pop();
                marks.insert(id.clone(), 2);
                order.push(id);
                continue;
            }
            let next = dependencies[*index].clone();
            *index += 1;
            if !graph.contains_key(&next) {
                continue;
            }
            match marks.get(&next) {
                Some(1) => {
                    let first = stack
                        .iter()
                        .position(|(id, _)| *id == next)
                        .expect("ancestor");
                    let mut cycle: Vec<_> =
                        stack[first..].iter().map(|(id, _)| id.clone()).collect();
                    cycle.push(next);
                    return Err(cycle);
                }
                Some(_) => {}
                None => {
                    marks.insert(next.clone(), 1);
                    stack.push((next, 0));
                }
            }
        }
    }
    Ok(order)
}

/// Data only, including file paths. Gates are deliberately absent from this function.
pub fn effective_inputs(
    plan: &Plan,
    state: &crate::gates::StateSnapshot,
    step: &Step,
) -> Option<IndexMap<String, EffectiveInput>> {
    step.bindings
        .iter()
        .map(|(name, binding)| {
            let value = match binding {
                Binding::File(path) => EffectiveInput::File(path.clone()),
                Binding::Default(value) => EffectiveInput::Value(value.clone()),
                Binding::Source(reference) => {
                    match crate::gates::resolve_reference(plan, state, reference) {
                        crate::types::BoundValue::Ready(value) => EffectiveInput::Value(value),
                        _ => return None,
                    }
                }
                Binding::Sources(references) => {
                    let values: Option<Vec<_>> = references
                        .iter()
                        .map(|reference| {
                            match crate::gates::resolve_reference(plan, state, reference) {
                                crate::types::BoundValue::Ready(value) => {
                                    Some(value.as_value().clone())
                                }
                                _ => None,
                            }
                        })
                        .collect();
                    EffectiveInput::Value(
                        JsonValue::try_from(Value::Array(values?)).expect("strict resolved values"),
                    )
                }
            };
            Some((name.clone(), value))
        })
        .collect()
}
pub fn inputs_hash(
    plan: &Plan,
    state: &crate::gates::StateSnapshot,
    step: &Step,
) -> Option<InputsHash> {
    InputsHash::from_bindings(&effective_inputs(plan, state, step)?).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceLimit {
    Fixed(u64),
    Dynamic,
}
/// Validate resource declarations only for changed needs, so lowering a capacity
/// does not invalidate unrelated edits. Pass None for initial plan validation.
pub fn validate_changed_needs(
    before: Option<&Plan>,
    after: &Plan,
    resources: &IndexMap<String, ResourceLimit>,
) -> Result<(), Vec<PathError>> {
    let mut errors = vec![];
    for (id, step) in after.steps() {
        if before
            .and_then(|plan| plan.steps().get(id))
            .is_some_and(|old| old.needs == step.needs)
        {
            continue;
        }
        errors.extend(needs_errors(step, resources));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
/// A step's `needs` against the project's resources.
pub(crate) fn needs_errors(
    step: &Step,
    resources: &IndexMap<String, ResourceLimit>,
) -> Vec<PathError> {
    let mut errors = vec![];
    for (name, need) in &step.needs {
        let path = format!("steps.{}.needs.{name}", step.id);
        match resources.get(name) {
            None => errors.push(diagnostic(
                &path,
                format!("the project declares no resource {name}"),
            )),
            Some(ResourceLimit::Fixed(capacity)) if need > capacity => errors.push(diagnostic(
                &path,
                format!("asks for {need}, more than {name}'s capacity {capacity}"),
            )),
            _ => {}
        }
    }
    errors
}
