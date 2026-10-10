//! Parsed plan semantics. This module never reads files or invokes a function.

use crate::{
    commands::{EditPreview, EditResult, PatchOperation, RuntimeApi},
    edit::PreparedEdit,
    error::PublicError,
    gates::{Gate, Reference, ValueRef},
    hash::{EffectiveInput, InputsHash},
    ids::{Revision, StepId, UnitName},
    rpc::{JsonMap, JsonValue, decode_json},
    types::{PathError, Type, check_value_at, fits, navigate},
    units::{Unit, derive_units},
};
use indexmap::{IndexMap, IndexSet};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub revision: Revision,
    pub document: JsonMap,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanPatchData {
    pub expected: Revision,
    pub ops: Vec<PatchOperation>,
}
/// Unvalidated wire document. Compile with a signature provider before use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct PlanDocument(JsonMap);
pub async fn apply_edit(
    _api: &impl RuntimeApi,
    _edit: PreparedEdit,
) -> Result<EditResult, PublicError> {
    Err(PublicError::not_implemented("apply_edit"))
}

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
}

/// Only whole-plan validation can construct this compiled, acyclic certificate.
/// Queries retain plan order. Deserialize wire data as `PlanDocument` first.
///
/// ```compile_fail
/// let plan: sluice_model::Plan =
///     sluice_model::rpc::decode_json(br#"{"steps":{}}"#).unwrap();
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    document: JsonMap,
    inputs: IndexMap<String, Declaration>,
    outputs: IndexMap<String, ValueRef>,
    steps: IndexMap<StepId, Step>,
    units: IndexMap<UnitName, Unit>,
    dependencies: IndexMap<StepId, Vec<StepId>>,
    order: Vec<StepId>,
}
impl Serialize for Plan {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.document.serialize(serializer)
    }
}
impl JsonSchema for Plan {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Plan".into()
    }
    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        PlanDocument::json_schema(g)
    }
}
impl PlanDocument {
    pub fn compile(&self, signatures: &impl SignatureProvider) -> Result<Plan, Vec<PathError>> {
        Plan::parse(&self.0, signatures)
    }
}
impl Plan {
    pub fn parse_json(
        bytes: &[u8],
        signatures: &impl SignatureProvider,
    ) -> Result<Self, Vec<PathError>> {
        let document =
            decode_json(bytes).map_err(|error| vec![diagnostic("plan", error.to_string())])?;
        parse_plan(document, signatures)
    }
    pub fn parse(
        document: &JsonMap,
        signatures: &impl SignatureProvider,
    ) -> Result<Self, Vec<PathError>> {
        parse_plan(document.clone(), signatures)
    }
    /// `parse`, keeping the document it is given rather than a copy of it.
    pub fn parse_owned(
        document: JsonMap,
        signatures: &impl SignatureProvider,
    ) -> Result<Self, Vec<PathError>> {
        parse_plan(document, signatures)
    }
    pub fn inputs(&self) -> &IndexMap<String, Declaration> {
        &self.inputs
    }
    pub fn outputs(&self) -> &IndexMap<String, ValueRef> {
        &self.outputs
    }
    pub fn steps(&self) -> &IndexMap<StepId, Step> {
        &self.steps
    }
    pub fn units(&self) -> &IndexMap<UnitName, Unit> {
        &self.units
    }
    pub fn dependencies(&self, id: &StepId) -> &[StepId] {
        self.dependencies
            .get(id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
    pub fn topological_order(&self) -> &[StepId] {
        &self.order
    }
    pub fn document(&self) -> &JsonMap {
        &self.document
    }
    pub fn transport(&self) -> PlanDocument {
        PlanDocument(self.document.clone())
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
    /// RFC 6902 patch on a copy, then whole-graph validation. Failure leaves self untouched.
    pub fn patch(
        &self,
        ops: &[PatchOperation],
        signatures: &impl SignatureProvider,
    ) -> Result<Self, Vec<PathError>> {
        // The document as a JSON object, moved rather than serialized out of a copy.
        let mut value = Value::Object(
            self.document
                .0
                .iter()
                .map(|(key, value)| (key.clone(), value.as_value().clone()))
                .collect(),
        );
        for (index, operation) in ops.iter().enumerate() {
            let path = match operation {
                PatchOperation::Add { path, .. }
                | PatchOperation::Remove { path }
                | PatchOperation::Replace { path, .. }
                | PatchOperation::Move { path, .. }
                | PatchOperation::Copy { path, .. }
                | PatchOperation::Test { path, .. } => path,
            };
            // serde_json::Map::remove uses swap removal with preserve_order. Keep the key
            // order of the maps above each touched path, not a copy of the whole plan, and
            // restore those maps only, leaving replacement subtrees in supplied order.
            let mut orders = ancestor_orders(&value, path);
            if let PatchOperation::Move { from, .. } = operation {
                orders.extend(ancestor_orders(&value, from));
            }
            let patch: json_patch::Patch = serde_json::from_value(
                serde_json::to_value(std::slice::from_ref(operation))
                    .map_err(|error| vec![diagnostic("ops", error.to_string())])?,
            )
            .map_err(|error| vec![diagnostic("ops", error.to_string())])?;
            json_patch::patch(&mut value, &patch)
                .map_err(|error| vec![diagnostic(&format!("ops[{index}]"), error.to_string())])?;
            for (parent, keys) in orders {
                restore_order(&mut value, &parent, &keys);
            }
        }
        Self::parse_owned(json_map(value)?, signatures)
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

/// A patched document as a strict map, its values moved in rather than deserialized
/// from a copy. Anything `JsonValue` would refuse is converted as before, for the same
/// refusal.
fn json_map(value: Value) -> Result<JsonMap, Vec<PathError>> {
    match value {
        Value::Object(map) if map.values().all(crate::rpc::strict_value) => Ok(JsonMap(
            map.into_iter()
                .map(|(key, value)| {
                    (
                        key,
                        JsonValue::try_from(value).expect("checked strict JSON"),
                    )
                })
                .collect(),
        )),
        value => serde_json::from_value(value)
            .map_err(|error| vec![diagnostic("plan", error.to_string())]),
    }
}
/// The key order of every map above `path`, outermost first.
fn ancestor_orders(value: &Value, path: &str) -> Vec<(String, Vec<String>)> {
    path.match_indices('/')
        .filter_map(|(offset, _)| {
            let parent = &path[..offset];
            let map = value.pointer(parent)?.as_object()?;
            Some((parent.to_owned(), map.keys().cloned().collect()))
        })
        .collect()
}
/// Put the map at `parent` back in `keys` order, keys it did not have after them.
fn restore_order(value: &mut Value, parent: &str, keys: &[String]) {
    if let Some(new) = value.pointer_mut(parent).and_then(Value::as_object_mut) {
        let known: std::collections::HashSet<&str> = keys.iter().map(String::as_str).collect();
        let added: Vec<String> = new
            .keys()
            .filter(|key| !known.contains(key.as_str()))
            .cloned()
            .collect();
        let mut remaining = std::mem::take(new);
        for key in keys.iter().chain(&added) {
            if let Some(value) = remaining.swap_remove(key) {
                new.insert(key.clone(), value);
            }
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
    closed_keys(raw.keys(), keys, path, errors);
}
fn closed_keys<'k>(
    present: impl Iterator<Item = &'k String>,
    keys: &[&str],
    path: &str,
    errors: &mut Vec<PathError>,
) {
    for key in present.filter(|key| !keys.contains(&key.as_str())) {
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
fn valid_id(name: &str, path: &str, errors: &mut Vec<PathError>) -> bool {
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
fn binding(raw: &Value, path: &str, errors: &mut Vec<PathError>) -> Option<Binding> {
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

fn parse_step(
    id: StepId,
    raw: &Value,
    signatures: &impl SignatureProvider,
    errors: &mut Vec<PathError>,
) -> Option<Step> {
    let path = format!("steps.{id}");
    let raw = object(raw, &path, errors)?;
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
    })
}

/// The document is read in place, top-level entry by entry, and kept by the plan.
fn parse_plan(
    document: JsonMap,
    signatures: &impl SignatureProvider,
) -> Result<Plan, Vec<PathError>> {
    fn top<'d>(document: &'d JsonMap, key: &str) -> Option<&'d Value> {
        document.0.get(key).map(JsonValue::as_value)
    }
    crate::types::validate_json_map(&document, "plan")?;
    let mut errors = Vec::new();
    closed_keys(
        document.0.keys(),
        &["inputs", "outputs", "steps"],
        "",
        &mut errors,
    );
    let inputs = top(&document, "inputs")
        .map(|value| declarations(value, "inputs", &mut errors))
        .unwrap_or_default();
    let mut steps = IndexMap::new();
    match top(&document, "steps") {
        None => errors.push(diagnostic("steps", "required, an object of id -> step")),
        Some(value) => {
            if let Some(map) = object(value, "steps", &mut errors) {
                for (name, value) in map {
                    let path = format!("steps.{name}");
                    if !valid_id(name, &path, &mut errors) {
                        continue;
                    }
                    if ["owner", "orchestrator"].contains(&name.as_str()) {
                        errors.push(diagnostic(&path, "reserved message address"));
                    }
                    if inputs.contains_key(name) {
                        errors.push(diagnostic(
                            &path,
                            "plan inputs and steps share one namespace",
                        ));
                    }
                    if let Some(step) = parse_step(
                        name.parse().expect("checked id"),
                        value,
                        signatures,
                        &mut errors,
                    ) {
                        steps.insert(step.id.clone(), step);
                    }
                }
            }
        }
    }
    let mut plan = Plan {
        document,
        inputs,
        outputs: IndexMap::new(),
        steps,
        units: IndexMap::new(),
        dependencies: IndexMap::new(),
        order: vec![],
    };
    if let Some(value) = top(&plan.document, "outputs").cloned()
        && let Some(map) = object(&value, "outputs", &mut errors)
    {
        for (name, value) in map {
            let path = format!("outputs.{name}");
            valid_id(name, &path, &mut errors);
            if let Some(Binding::Source(reference)) = binding(value, &path, &mut errors) {
                if let Err(error) = plan.reference_type(&reference) {
                    errors.push(diagnostic(&path, error));
                }
                plan.outputs.insert(name.clone(), reference);
            } else {
                errors.push(diagnostic(&path, "expected {\"source\": \"<ref>\"}"));
            }
        }
    }
    let mut compiled_extras = Vec::new();
    for (id, step) in &plan.steps {
        let mut extras = IndexMap::new();
        for (name, binding) in &step.bindings {
            let path = format!("steps.{id}.in.{name}");
            if let Some(target) = step.signature.inputs.get(name) {
                let target = if step.scatter.as_ref() == Some(name) {
                    Type::List(Box::new(target.clone()))
                } else {
                    target.clone()
                };
                check_binding(binding, &target, &plan, &path, &mut errors);
            } else if step.signature.open {
                let mut ty = binding_type(binding, &plan, &path, &mut errors);
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
        compiled_extras.push((id.clone(), extras));
    }
    for (id, extra) in compiled_extras {
        plan.steps.get_mut(&id).expect("known step").extra_inputs = extra;
    }
    let mut compiled = Vec::new();
    for id in plan.steps.keys() {
        let mut gates = Vec::new();
        if let Some(entries) = top(&plan.document, "steps")
            .and_then(|steps| steps.get(id.as_str()))
            .and_then(|step| step.get("after"))
            .and_then(Value::as_array)
        {
            for (index, entry) in entries.iter().enumerate() {
                if let Some(text) = entry.as_str() {
                    match Gate::compile(text, &plan) {
                        Ok(gate) => {
                            if !gates.contains(&gate) {
                                gates.push(gate);
                            }
                        }
                        Err(error) => {
                            errors.push(diagnostic(&format!("steps.{id}.after[{index}]"), error))
                        }
                    }
                }
            }
        }
        compiled.push((id.clone(), gates));
    }
    for (id, gates) in compiled {
        plan.steps.get_mut(&id).expect("known step").after = gates;
    }
    // Invalid tags cannot enter invariant-bearing UnitName constructors.
    if plan.steps.values().any(|step| {
        step.tags.iter().any(|tag| {
            tag.strip_prefix("unit:")
                .is_some_and(|name| UnitName::new(name).is_err())
        })
    }) {
        return Err(errors);
    }
    for (id, step) in &plan.steps {
        if !step.tags.iter().any(|tag| tag.starts_with("unit:"))
            && plan
                .steps
                .values()
                .any(|other| other.tags.iter().any(|tag| tag == &format!("unit:{id}")))
        {
            errors.push(diagnostic(
                &format!("steps.{id}.tags"),
                "singleton unit name collides with a tagged unit",
            ));
        }
    }
    plan.units = derive_units(&plan);
    for step in plan.steps.values() {
        for gate in &step.after {
            if let Gate::Unit { name, .. } = gate {
                if !plan.units.contains_key(name) {
                    errors.push(diagnostic(
                        &format!("steps.{}.after", step.id),
                        format!("no unit {name}"),
                    ));
                }
                if *name == step.unit_name() {
                    errors.push(diagnostic(
                        &format!("steps.{}.after", step.id),
                        "a unit cannot depend on its own exits",
                    ));
                }
            }
        }
    }
    plan.dependencies = expanded_dependencies(&plan);
    // Exits/entries depend on the expanded internal graph, including every gate form.
    plan.units = derive_units(&plan);
    match topological_order(&plan.dependencies) {
        Ok(order) => plan.order = order,
        Err(cycle) => errors.push(diagnostic(
            &format!("steps.{}", cycle[0]),
            format!(
                "dependency cycle {}",
                cycle
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
        )),
    }
    if errors.is_empty() {
        Ok(plan)
    } else {
        Err(errors)
    }
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

pub(crate) fn expanded_dependencies(plan: &Plan) -> IndexMap<StepId, Vec<StepId>> {
    plan.steps
        .iter()
        .map(|(id, step)| {
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
            (id.clone(), dependencies.into_iter().collect())
        })
        .collect()
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
        for (name, need) in &step.needs {
            let path = format!("steps.{id}.needs.{name}");
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
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Prepare an atomic plan patch and its shared dry-run preview. The coordinator
/// must recheck expected revision when committing this result. No state is written.
pub(crate) fn prepare_patch(
    revision: Revision,
    before: &Plan,
    state: &crate::gates::StateSnapshot,
    edit: PlanPatchData,
    signatures: &impl SignatureProvider,
    resources: &crate::gates::CachedResources,
    limits: &IndexMap<String, ResourceLimit>,
) -> Result<(Plan, EditPreview, crate::gates::StateSnapshot), PublicError> {
    if edit.expected != revision {
        return Err(PublicError::Conflict {
            message: "plan revision changed".into(),
            current_rev: Some(revision),
        });
    }
    let invalid = |errors: Vec<PathError>| PublicError::Invalid {
        message: "invalid plan edit".into(),
        errors: errors.iter().map(ToString::to_string).collect(),
    };
    let after = before.patch(&edit.ops, signatures).map_err(invalid)?;
    let mut errors = vec![];
    for (id, step) in before.steps() {
        if state.status(id) != crate::commands::StepStatus::Running {
            continue;
        }
        let allowed = after.steps().get(id).is_some_and(|new| {
            let mut old = step.clone();
            let mut new = new.clone();
            old.paused = Pause::No;
            new.paused = Pause::No;
            old.tags.clear();
            new.tags.clear();
            old == new
        });
        if !allowed {
            errors.push(diagnostic(
                &format!("steps.{id}"),
                "cannot remove or change a running step except paused and tags",
            ));
        }
    }
    if let Err(found) = validate_changed_needs(Some(before), &after, limits) {
        errors.extend(found);
    }
    let mut projected = state.clone();
    // Only ids present before and after this one patch retain their state.
    projected
        .steps
        .retain(|id, _| before.steps().contains_key(id));
    projected
        .inputs
        .0
        .retain(|name, _| before.inputs().contains_key(name) && after.inputs().contains_key(name));
    if let Err(found) = after.validate_input_values(&projected.inputs) {
        errors.extend(found);
    }
    if !errors.is_empty() {
        return Err(invalid(errors));
    }
    let preview = crate::gates::simulate_edit(before, state, &after, &projected, resources);
    Ok((
        after,
        EditPreview {
            ops: edit.ops,
            would_start: preview.would_start,
            would_queue: preview.would_queue.into_keys().collect(),
            would_skip: preview.would_skip.into_keys().collect(),
            would_stale: preview.would_stale,
            errors: preview.errors.iter().map(ToString::to_string).collect(),
        },
        preview.reconciled,
    ))
}

// ---- lane D seams: lane C's pinned model API (docs/design/plan-rows.md §8) -------------
//
// Lane D (runtime) codes against these signatures as the contract pins them; lane C's
// implementation replaces this block on the integration branch (rw/pn-cutover).

/// Compile the plan from its rows, never through a document (a cold compile).
pub fn compile_rows(
    _rows: &crate::plan_rows::PlanRows,
    _signatures: &impl SignatureProvider,
) -> Result<Plan, Vec<PathError>> {
    todo!("lane C: compile_rows (plan-rows §8)")
}

/// A step's rebuildable index rows from its declaration alone (plan-rows §2.5).
pub fn step_index(
    _step: &StepId,
    _declaration: &JsonMap,
    _is_step: &dyn Fn(&str) -> bool,
) -> crate::plan_rows::StepIndexRows {
    todo!("lane C: step_index (plan-rows §2.5, §8)")
}

/// A plan output's `plan_refs` row (plan-rows §2.5).
pub fn output_references(_name: &str, _binding: &JsonMap) -> Vec<crate::plan_rows::ReferenceRow> {
    todo!("lane C: output_references (plan-rows §2.5, §8)")
}
