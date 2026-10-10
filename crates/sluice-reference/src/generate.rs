//! Generators for differential tests: plans, runtime states, `PlanOp` batches and typed edit
//! commands over one fixed catalog of fns and recipes.
//!
//! Every generator draws plain bytes and builds its value from them deterministically
//! (`build_*`), so a failing case shrinks toward byte 0, the simplest choice everywhere, and
//! the same bytes build the same case in any crate. A generated base always compiles (with
//! the reference and with today's `sluice-model`); generated operations are mostly
//! well-aimed and often invalid on purpose: dangling references after a removal, cycles
//! (also ones two operations only make together), unit-gate cycles, singleton collisions,
//! changed output types under distant readers, input and output puts, recipe overrides, a fn
//! the base never used, running steps, `start: false` and order changes.

use crate::{
    commands::StepStatus,
    gates::{CachedResources, StateSnapshot, StepState},
    harness::{Context, EditCase, header},
    ids::{Revision, StepId},
    ops::{self, document_of, unit_of},
    plan::{Declaration, FnSignature, Pause, Plan, ResourceLimit, inputs_hash},
    recipe::{RecipeEntry, catalog as recipe_catalog},
    rpc::{JsonMap, JsonValue},
    types::{SkipReason, Type, fits, navigate},
};
use indexmap::IndexMap;
use proptest::prelude::*;
use serde_json::{Map, Value, json};
use sluice_model::{
    hash::InputsHash,
    plan_rows::{OrderCollection, PlanOp, PlanRows, StepChanges},
};

/// The fns the generated plans run. `t.late` is never used by a generated base, so an
/// operation can add a step with a fn the base never used.
pub fn catalog() -> IndexMap<String, FnSignature> {
    fn types(value: Value) -> IndexMap<String, Type> {
        value
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, form)| (name.clone(), Type::parse(form).expect("a type")))
            .collect()
    }
    let fun = |inputs: Value, outputs: Value, submits: Value, open: bool| FnSignature {
        inputs: types(inputs),
        outputs: types(outputs),
        submits: types(submits)
            .into_iter()
            .map(|(name, ty)| (name, Declaration { ty, doc: None }))
            .collect(),
        open,
    };
    IndexMap::from([
        (
            "t.add".into(),
            fun(
                json!({"a":"int","b":"int"}),
                json!({"sum":"int"}),
                json!({}),
                false,
            ),
        ),
        (
            "t.text".into(),
            fun(
                json!({"text":"string","tag":"string?"}),
                json!({"text":"string","words":"string[]"}),
                json!({}),
                false,
            ),
        ),
        (
            "t.flag".into(),
            fun(
                json!({}),
                json!({"ok":"boolean","n":"int"}),
                json!({}),
                false,
            ),
        ),
        (
            "t.info".into(),
            fun(
                json!({"seed":"int?"}),
                json!({"info":{"type":"record","fields":{"ok":"boolean","count":"int","name":"string?"}}}),
                json!({}),
                false,
            ),
        ),
        (
            "t.collect".into(),
            fun(
                json!({"items":"Any[]"}),
                json!({"items":"Any[]"}),
                json!({}),
                false,
            ),
        ),
        (
            "t.echo".into(),
            fun(
                json!({"value":"Any"}),
                json!({"value":"Any"}),
                json!({}),
                false,
            ),
        ),
        (
            "t.review".into(),
            fun(
                json!({"spec":"string"}),
                json!({}),
                json!({"verdict":"boolean","notes":"string?"}),
                false,
            ),
        ),
        (
            "t.open".into(),
            fun(
                json!({"attempts":"Any[]?"}),
                json!({"results":"int[]"}),
                json!({}),
                true,
            ),
        ),
        (
            "core.external".into(),
            fun(json!({}), json!({}), json!({}), true),
        ),
        (
            "t.late".into(),
            fun(json!({"x":"int"}), json!({"y":"int"}), json!({}), false),
        ),
    ])
}
/// The fns a generated base uses (every one but `t.late`).
const BASE_FNS: [&str; 9] = [
    "t.add",
    "t.text",
    "t.flag",
    "t.info",
    "t.collect",
    "t.echo",
    "t.review",
    "t.open",
    "core.external",
];

/// The recipes `unit.add` draws from: (name, scope, JSON). `broken` does not parse, so its
/// entry is an error; `outside` reads the plan input `repo`, an external reference.
pub const RECIPES: &[(&str, &str, &str)] = &[
    (
        "lane",
        "global",
        r#"{"name":"lane","params":{"ticket":"string"},"title":"{ticket}","steps":{
            "{unit}-fork":{"run":"t.text","in":{"text":{"default":"{ticket}"}}},
            "{unit}-work":{"run":"t.add","in":{"a":{"default":1},"b":{"default":2}},"after":["{unit}-fork"]},
            "{unit}-land":{"run":"t.flag","after":["{unit}-work"]}}}"#,
    ),
    (
        "pair",
        "project",
        r#"{"name":"pair","params":{"n":"int?"},"steps":{
            "{unit}-a":{"run":"t.echo","in":{"value":{"default":"{n}"}}},
            "{unit}-b":{"run":"t.collect","in":{"items":{"source":["{unit}-a/value"]}}}}}"#,
    ),
    (
        "solo",
        "global",
        r#"{"name":"solo","steps":{"{unit}":{"run":"t.flag","tags":["solo"]}}}"#,
    ),
    (
        "outside",
        "project",
        r#"{"name":"outside","steps":{"{unit}-use":{"run":"t.text","in":{"text":{"source":"repo"}}}}}"#,
    ),
    ("broken", "project", r#"{"name":"broken","steps":{}}"#),
];
pub fn recipes() -> IndexMap<String, RecipeEntry> {
    recipe_catalog(RECIPES.iter().map(|(n, s, j)| (*n, *s, j.as_bytes())))
}
/// The resources generated steps need: `r1` fixed at 2, `r2` dynamic.
pub fn limits() -> IndexMap<String, ResourceLimit> {
    IndexMap::from([
        ("r1".into(), ResourceLimit::Fixed(2)),
        ("r2".into(), ResourceLimit::Dynamic),
    ])
}

/// The plan inputs a base may declare, with their declarations.
const INPUTS: [(&str, &str); 6] = [
    ("repo", r#""string""#),
    ("limit", r#""int""#),
    ("enabled", r#"{"type":"boolean","doc":"Run it"}"#),
    (
        "cfg",
        r#"{"type":{"type":"record","fields":{"enabled":"boolean?","n":"int"}}}"#,
    ),
    ("names", r#""string[]""#),
    ("note", r#""string?""#),
];

/// Bytes read in a cycle; an empty source reads zeroes.
struct Bytes<'a> {
    data: &'a [u8],
    at: usize,
}
impl<'a> Bytes<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }
    fn next(&mut self) -> u8 {
        if self.data.is_empty() {
            return 0;
        }
        let b = self.data[self.at % self.data.len()];
        self.at += 1;
        b
    }
    fn pick<'t, T>(&mut self, items: &'t [T]) -> Option<&'t T> {
        if items.is_empty() {
            None
        } else {
            let b = self.next() as usize;
            items.get(b % items.len())
        }
    }
    fn chance(&mut self, one_in: u8) -> bool {
        self.next().is_multiple_of(one_in)
    }
}

/// A value of `ty`, from `b`.
pub fn value(ty: &Type, b: u8) -> Value {
    match ty {
        Type::String => json!(format!("v{}", b % 10)),
        Type::Int => json!(i64::from(b % 10)),
        Type::Float => json!(f64::from(b % 10) + 0.5),
        Type::Boolean => json!(b.is_multiple_of(2)),
        Type::Any => json!(b % 10),
        Type::Optional(inner) => {
            if b % 4 == 3 {
                Value::Null
            } else {
                value(inner, b)
            }
        }
        Type::List(inner) => {
            if b % 5 == 4 {
                json!([])
            } else {
                json!([value(inner, b), value(inner, b.wrapping_add(1))])
            }
        }
        Type::Enum(symbols) => json!(symbols[b as usize % symbols.len()]),
        Type::Record(fields) => Value::Object(
            fields
                .iter()
                .enumerate()
                .map(|(i, (name, ty))| (name.clone(), value(ty, b.wrapping_add(i as u8))))
                .collect(),
        ),
    }
}

/// A step's declared outputs and their types, read from its declaration without compiling:
/// its fn's outputs and submits, then its own `outputs`, each a list when it scatters.
fn output_types(
    declaration: &Map<String, Value>,
    catalog: &IndexMap<String, FnSignature>,
) -> Vec<(String, Type)> {
    let mut out = vec![];
    if let Some(signature) = declaration
        .get("run")
        .and_then(Value::as_str)
        .and_then(|run| catalog.get(run))
    {
        out.extend(
            signature
                .outputs
                .iter()
                .map(|(n, t)| (n.clone(), t.clone())),
        );
        out.extend(
            signature
                .submits
                .iter()
                .map(|(n, d)| (n.clone(), d.ty.clone())),
        );
    }
    if let Some(declared) = declaration.get("outputs").and_then(Value::as_object) {
        for (name, form) in declared {
            let form = match form {
                Value::Object(map) if map.contains_key("type") => &map["type"],
                other => other,
            };
            if let Ok(ty) = Type::parse(form) {
                out.push((name.clone(), ty));
            }
        }
    }
    if declaration.contains_key("scatter") {
        out = out
            .into_iter()
            .map(|(n, t)| (n, Type::List(Box::new(t))))
            .collect();
    }
    out
}
/// Every ref into a value of type `ty` at `base` (the value itself, a record's fields, a
/// list's first item), with its type.
fn refs(base: &str, ty: &Type) -> Vec<(String, Type)> {
    let mut out = vec![(base.to_owned(), ty.clone())];
    let mut inner = ty;
    while let Type::Optional(t) = inner {
        inner = t;
    }
    match inner {
        Type::Record(fields) => {
            for name in fields.keys() {
                if let Ok(t) = navigate(ty, name) {
                    out.push((format!("{base}.{name}"), t));
                }
            }
        }
        Type::List(_) => {
            if let Ok(t) = navigate(ty, "0") {
                out.push((format!("{base}.0"), t));
            }
        }
        _ => {}
    }
    out
}

/// A step a builder can read: its id, its unit, and every ref into its outputs with its type.
type Visible = (String, String, Vec<(String, Type)>);

/// What a step builder can read: the plan inputs and the steps before it.
struct Scope<'a> {
    catalog: &'a IndexMap<String, FnSignature>,
    /// Plan input refs with their types.
    inputs: Vec<(String, Type)>,
    /// Earlier steps: id, unit, and every ref into their outputs with its type.
    steps: Vec<Visible>,
}
impl<'a> Scope<'a> {
    fn of(document: &Map<String, Value>, catalog: &'a IndexMap<String, FnSignature>) -> Self {
        let mut scope = Self {
            catalog,
            inputs: vec![],
            steps: vec![],
        };
        for (name, form) in document
            .get("inputs")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let form = match form {
                Value::Object(map) if map.contains_key("type") => &map["type"],
                other => other,
            };
            if let Ok(ty) = Type::parse(form) {
                scope.inputs.extend(refs(name, &ty));
            }
        }
        for (id, declaration) in document
            .get("steps")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if let Some(declaration) = declaration.as_object() {
                scope.add(id, declaration);
            }
        }
        scope
    }
    fn add(&mut self, id: &str, declaration: &Map<String, Value>) {
        let map: JsonMap = serde_json::from_value(Value::Object(declaration.clone()))
            .expect("a strict declaration");
        let refs = output_types(declaration, self.catalog)
            .iter()
            .flat_map(|(name, ty)| refs(&format!("{id}/{name}"), ty))
            .collect();
        self.steps.push((id.to_owned(), unit_of(id, &map), refs));
    }
    fn all_refs(&self) -> Vec<(String, Type)> {
        self.inputs
            .iter()
            .cloned()
            .chain(self.steps.iter().flat_map(|(_, _, r)| r.iter().cloned()))
            .collect()
    }
    /// A binding for an input of type `target`.
    fn binding(&self, target: &Type, bytes: &mut Bytes) -> Value {
        let b = bytes.next();
        let mut options = vec![json!({"default": value(target, b)})];
        if fits(&Type::String, target) {
            options.push(json!({"file": format!("/tmp/in-{}", b % 4)}));
        }
        for (reference, ty) in self.all_refs() {
            if fits(&ty, target) {
                options.push(json!({"source": reference}));
            }
        }
        let mut list = target;
        while let Type::Optional(t) = list {
            list = t;
        }
        let item = match list {
            Type::List(item) => Some(item.as_ref().clone()),
            Type::Any => Some(Type::Any),
            _ => None,
        };
        if let Some(item) = item {
            let fitting: Vec<_> = self
                .all_refs()
                .into_iter()
                .filter(|(_, ty)| fits(ty, &item))
                .map(|(r, _)| r)
                .collect();
            if let (Some(first), Some(second)) = (bytes.pick(&fitting), bytes.pick(&fitting)) {
                options.push(json!({"source": [first, second]}));
            }
        }
        bytes.pick(&options).cloned().expect("a default")
    }
    /// Gate entries on earlier steps, other units and boolean refs.
    fn gates(&self, own_unit: &str, bytes: &mut Bytes) -> Vec<String> {
        let mut options: Vec<String> = vec![];
        for (id, unit, refs) in &self.steps {
            options.push(id.clone());
            options.push(format!("{id}?"));
            if unit != own_unit {
                options.push(format!("unit:{unit}"));
                options.push(format!("unit:{unit}?"));
            }
            for (reference, ty) in refs {
                let mut inner = ty;
                while let Type::Optional(t) = inner {
                    inner = t;
                }
                if matches!(inner, Type::Boolean | Type::Any) {
                    options.push(reference.clone());
                    options.push(format!("!{reference}"));
                }
            }
        }
        for (reference, ty) in &self.inputs {
            let mut inner = ty;
            while let Type::Optional(t) = inner {
                inner = t;
            }
            if matches!(inner, Type::Boolean) {
                options.push(reference.clone());
                options.push(format!("!{reference}"));
            }
        }
        let count = bytes.next() % 3;
        let mut gates: Vec<String> = vec![];
        for _ in 0..count {
            if let Some(gate) = bytes.pick(&options)
                && !gates.contains(gate)
            {
                gates.push(gate.clone());
            }
        }
        gates
    }
    /// A step declaration running `run`, in unit `unit` (tagged when `tagged`).
    fn step(
        &self,
        run: &str,
        unit: Option<&str>,
        own_unit: &str,
        bytes: &mut Bytes,
    ) -> Map<String, Value> {
        let signature = &self.catalog[run];
        let mut declaration = Map::new();
        declaration.insert("run".into(), json!(run));
        let mut bindings = Map::new();
        for (name, ty) in &signature.inputs {
            if matches!(ty, Type::Optional(_)) && bytes.chance(2) {
                continue;
            }
            bindings.insert(name.clone(), self.binding(ty, bytes));
        }
        let mut scatter = None;
        if signature.open {
            for k in 0..bytes.next() % 3 {
                bindings.insert(format!("x{k}"), self.binding(&Type::Any, bytes));
            }
            if run != "core.external" && bytes.chance(4) {
                let list = Type::List(Box::new(Type::Any));
                bindings.insert("items".into(), self.binding(&list, bytes));
                if bindings["items"].get("default").is_none_or(Value::is_array)
                    && bindings["items"]
                        .get("source")
                        .is_none_or(|s| s.is_array() || self.is_list_ref(s))
                    && !bindings["items"].as_object().unwrap().contains_key("file")
                {
                    scatter = Some("items");
                }
            }
        }
        if !bindings.is_empty() || bytes.chance(3) {
            declaration.insert("in".into(), Value::Object(bindings));
        }
        if let Some(name) = scatter {
            declaration.insert("scatter".into(), json!(name));
        }
        if signature.open {
            match bytes.next() % 4 {
                1 => {
                    declaration.insert("outputs".into(), json!({"flag":"boolean"}));
                }
                2 => {
                    declaration.insert(
                        "outputs".into(),
                        json!({"n":"int","info":{"type":"record","fields":{"ok":"boolean","count":"int"}}}),
                    );
                }
                3 => {
                    declaration.insert(
                        "outputs".into(),
                        json!({"flag":{"type":"boolean","doc":"Whether it held"}}),
                    );
                }
                _ => {}
            }
        }
        let mut tags = vec![];
        if let Some(unit) = unit {
            tags.push(format!("unit:{unit}"));
            if bytes.chance(3) {
                tags.push("exit".into());
            }
        }
        match bytes.next() % 5 {
            1 => tags.push("wave-1".into()),
            2 => tags.push("arc:x".into()),
            _ => {}
        }
        if !tags.is_empty() {
            declaration.insert("tags".into(), json!(tags));
        }
        let gates = self.gates(own_unit, bytes);
        if !gates.is_empty() {
            declaration.insert("after".into(), json!(gates));
        }
        match bytes.next() % 9 {
            0 => {
                declaration.insert("paused".into(), json!(true));
            }
            1 => {
                declaration.insert("paused".into(), json!("waiting for review"));
            }
            _ => {}
        }
        if bytes.chance(4) {
            declaration.insert("priority".into(), json!(i64::from(bytes.next() % 7) - 3));
        }
        match bytes.next() % 6 {
            0 => {
                declaration.insert("needs".into(), json!({"r1": 1}));
            }
            1 => {
                declaration.insert("needs".into(), json!({"r2": 2, "r1": 1}));
            }
            _ => {}
        }
        if bytes.chance(5) {
            declaration.insert("doc".into(), json!("What it does"));
        }
        declaration
    }
    fn is_list_ref(&self, source: &Value) -> bool {
        let Some(text) = source.as_str() else {
            return false;
        };
        self.all_refs().iter().any(|(r, ty)| {
            r == text && {
                let mut inner = ty;
                while let Type::Optional(t) = inner {
                    inner = t;
                }
                matches!(inner, Type::List(_))
            }
        })
    }
}

/// A base plan from bytes: some of `INPUTS`, steps `s0 …` each reading only what comes
/// before it (so it is acyclic), grouped into singleton and tagged units (`u0 …`), and plan
/// outputs. It always compiles.
pub fn build_plan(top: &[u8], steps: &[[u8; 16]]) -> JsonMap {
    let catalog = catalog();
    let mut top = Bytes::new(top);
    let mask = top.next();
    let mut inputs = Map::new();
    for (i, (name, form)) in INPUTS.iter().enumerate() {
        if mask & (1 << i) != 0 {
            inputs.insert((*name).into(), serde_json::from_str(form).unwrap());
        }
    }
    let mut document = Map::new();
    let layout = top.next();
    if !inputs.is_empty() || layout.is_multiple_of(2) {
        document.insert("inputs".into(), Value::Object(inputs));
    }
    let mut scope = Scope::of(&document, &catalog);
    let mut built = Map::new();
    let mut tagged: Option<String> = None;
    let mut units = 0;
    for (i, choice) in steps.iter().enumerate() {
        let mut bytes = Bytes::new(choice);
        let id = format!("s{i}");
        let run = *bytes.pick(&BASE_FNS).unwrap();
        let unit = match bytes.next() % 4 {
            0 if tagged.is_some() => tagged.clone(),
            1 => {
                units += 1;
                Some(format!("u{}", units - 1))
            }
            _ => None,
        };
        tagged = unit.clone();
        let own = unit.clone().unwrap_or_else(|| id.clone());
        let declaration = scope.step(run, unit.as_deref(), &own, &mut bytes);
        scope.add(&id, &declaration);
        built.insert(id, Value::Object(declaration));
    }
    let mut outputs = Map::new();
    let refs = scope.all_refs();
    for k in 0..top.next() % 3 {
        if let Some((reference, _)) = top.pick(&refs) {
            outputs.insert(format!("o{k}"), json!({"source": reference}));
        }
    }
    if !outputs.is_empty() || layout.is_multiple_of(3) {
        document.insert("outputs".into(), Value::Object(outputs));
    }
    if layout % 5 == 1 {
        let mut first = Map::new();
        first.insert("steps".into(), Value::Object(built));
        first.extend(document);
        document = first;
    } else {
        document.insert("steps".into(), Value::Object(built));
    }
    serde_json::from_value(Value::Object(document)).expect("a strict document")
}
/// Base plans of at most `max_steps` steps.
pub fn plan(max_steps: usize) -> impl Strategy<Value = JsonMap> {
    (
        prop::collection::vec(any::<u8>(), 4),
        prop::collection::vec(any::<[u8; 16]>(), 0..=max_steps),
    )
        .prop_map(|(top, steps)| build_plan(&top, &steps))
}

/// Runtime state for a compiled plan, from bytes: input values that fit their declarations,
/// each step pending, running, succeeded (outputs that fit, its inputs hash current or
/// stale), failed, stale or skipped, and sometimes the project paused.
pub fn build_state(plan: &Plan, data: &[u8]) -> StateSnapshot {
    let mut bytes = Bytes::new(data);
    let mut state = StateSnapshot::default();
    for (name, declaration) in plan.inputs() {
        if !bytes.chance(4) {
            let v = value(&declaration.ty, bytes.next());
            state
                .inputs
                .0
                .insert(name.clone(), JsonValue::try_from(v).expect("strict"));
        }
    }
    if bytes.chance(12) {
        state.paused = Pause::Reason("maintenance".into());
    }
    for id in plan.topological_order() {
        let step = &plan.steps()[id];
        let b = bytes.next();
        let status = match b % 9 {
            0..=2 => continue,
            3 | 4 => StepStatus::Succeeded,
            5 => StepStatus::Failed,
            6 => StepStatus::Running,
            7 => StepStatus::Skipped,
            _ => StepStatus::Stale,
        };
        let mut entry = StepState {
            status: status.clone(),
            ..Default::default()
        };
        if matches!(status, StepStatus::Succeeded | StepStatus::Stale) {
            let names: Vec<String> = step
                .signature
                .outputs
                .keys()
                .chain(step.declared_outputs.keys())
                .cloned()
                .collect();
            for name in names {
                let ty = step.output_type(&name).expect("an output");
                entry.outputs.0.insert(
                    name,
                    JsonValue::try_from(value(&ty, bytes.next())).expect("strict"),
                );
            }
            entry.inputs_hash = if b & 0x10 == 0 {
                inputs_hash(plan, &state, step)
            } else {
                Some(InputsHash::of(&JsonMap::default()).expect("a hash"))
            };
        }
        if status == StepStatus::Failed {
            entry.error = Some("it broke".into());
        }
        if status == StepStatus::Skipped {
            entry.skipped = vec![SkipReason::Step {
                step: plan
                    .dependencies(id)
                    .first()
                    .cloned()
                    .unwrap_or_else(|| id.clone()),
            }];
        }
        state.steps.insert(id.clone(), entry);
    }
    state
}

/// The steps, inputs, outputs and units of the candidate an operation is aimed at.
struct Target {
    document: Map<String, Value>,
    steps: Vec<String>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    units: IndexMap<String, Vec<String>>,
}
impl Target {
    fn of(rows: &PlanRows) -> Self {
        let document = match serde_json::to_value(document_of(rows)).unwrap() {
            Value::Object(map) => map,
            _ => unreachable!(),
        };
        let keys = |section: &str| -> Vec<String> {
            document
                .get(section)
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default()
        };
        let mut units = IndexMap::<String, Vec<String>>::new();
        for row in &rows.steps {
            units
                .entry(unit_of(row.step.as_str(), &row.declaration))
                .or_default()
                .push(row.step.to_string());
        }
        Self {
            steps: keys("steps"),
            inputs: keys("inputs"),
            outputs: keys("outputs"),
            units,
            document,
        }
    }
    fn declaration(&self, id: &str) -> Map<String, Value> {
        self.document
            .get("steps")
            .and_then(|steps| steps.get(id))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }
    /// A step id: usually an existing one, sometimes one that does not exist.
    fn step(&self, bytes: &mut Bytes) -> String {
        match bytes.pick(&self.steps) {
            Some(id) if !bytes.chance(16) => id.clone(),
            _ => "ghost".into(),
        }
    }
    fn unit(&self, bytes: &mut Bytes) -> String {
        let names: Vec<_> = self
            .units
            .keys()
            .filter(|name| crate::ids::UnitName::new(name.as_str()).is_ok())
            .cloned()
            .collect();
        match bytes.pick(&names) {
            Some(name) if !bytes.chance(16) => name.clone(),
            _ => "nowhere".into(),
        }
    }
    /// A scope over every step but `except` (the targets a declaration may read).
    fn scope<'a>(&self, catalog: &'a IndexMap<String, FnSignature>, except: &str) -> Scope<'a> {
        let mut document = self.document.clone();
        if let Some(Value::Object(steps)) = document.get_mut("steps") {
            steps.shift_remove(except);
        }
        Scope::of(&document, catalog)
    }
}
fn json_value(value: Value) -> JsonValue {
    JsonValue::try_from(value).expect("strict JSON")
}

/// One operation aimed at `target`, from bytes.
fn build_op(target: &Target, catalog: &IndexMap<String, FnSignature>, data: &[u8]) -> PlanOp {
    let mut bytes = Bytes::new(data);
    let fns: Vec<&str> = catalog.keys().map(String::as_str).collect();
    match bytes.next() % 13 {
        0 => {
            let names = ["repo", "limit", "extra", "Bad", "s0", "names"];
            let name = match bytes.pick(&target.inputs) {
                Some(name) if bytes.chance(2) => name.clone(),
                _ => (*bytes.pick(&names).unwrap()).to_owned(),
            };
            let forms = [
                json!("string"),
                json!("int"),
                json!("boolean"),
                json!("string[]"),
                json!({"type":"int","doc":"How many"}),
                json!({"type":"record","fields":{"enabled":"boolean?","n":"int"}}),
                json!("nope"),
            ];
            PlanOp::InputPut {
                name,
                declaration: json_value(bytes.pick(&forms).unwrap().clone()),
            }
        }
        1 => PlanOp::InputRemove {
            name: match bytes.pick(&target.inputs) {
                Some(name) if !bytes.chance(6) => name.clone(),
                _ => "missing".into(),
            },
        },
        2 => {
            let scope = target.scope(catalog, "");
            let refs: Vec<String> = scope.all_refs().into_iter().map(|(r, _)| r).collect();
            let source = match bytes.pick(&refs) {
                Some(r) if !bytes.chance(6) => r.clone(),
                _ => (*bytes.pick(&["ghost/out", "nosuch", "s0/nothing"]).unwrap()).to_owned(),
            };
            let name = match bytes.pick(&target.outputs) {
                Some(name) if bytes.chance(2) => name.clone(),
                _ => format!("o{}", bytes.next() % 4),
            };
            PlanOp::OutputPut { name, source }
        }
        3 => PlanOp::OutputRemove {
            name: match bytes.pick(&target.outputs) {
                Some(name) if !bytes.chance(6) => name.clone(),
                _ => "missing".into(),
            },
        },
        4 => {
            let names = ["n0", "n1", "u0", "repo", "s1"];
            let step = match bytes.next() % 6 {
                0 => target.step(&mut bytes),
                _ => (*bytes.pick(&names).unwrap()).to_owned(),
            };
            let scope = target.scope(catalog, &step);
            let run = *bytes.pick(&fns).unwrap();
            let unit = match bytes.next() % 4 {
                0 => Some(target.unit(&mut bytes)),
                _ => None,
            };
            let own = unit.clone().unwrap_or_else(|| step.clone());
            let spec = scope.step(run, unit.as_deref(), &own, &mut bytes);
            PlanOp::StepAdd {
                step: StepId::new(&step).unwrap(),
                spec: serde_json::from_value(Value::Object(spec)).unwrap(),
            }
        }
        5 => {
            let step = target.step(&mut bytes);
            PlanOp::StepUpdate {
                step: StepId::new(&step).unwrap(),
                changes: Box::new(build_changes(target, catalog, &step, &mut bytes)),
            }
        }
        6 => {
            let mut steps = vec![StepId::new(target.step(&mut bytes)).unwrap()];
            if bytes.chance(3) {
                let other = StepId::new(target.step(&mut bytes)).unwrap();
                if !steps.contains(&other) {
                    steps.push(other);
                }
            }
            PlanOp::StepRemove { steps }
        }
        7 | 8 => {
            let step = if bytes.chance(3) {
                format!("unit:{}", target.unit(&mut bytes))
            } else {
                target.step(&mut bytes)
            };
            let mut after = vec![];
            let existing: Vec<String> = target
                .declaration(&step)
                .get("after")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            let scope = target.scope(catalog, "");
            let mut entries = scope.gates("", &mut bytes);
            entries.extend(existing);
            if entries.is_empty() || bytes.chance(6) {
                entries.extend(["!s0".to_owned(), "s0/x?".to_owned(), "unit:gone".to_owned()]);
            }
            for _ in 0..1 + bytes.next() % 2 {
                let entry = bytes.pick(&entries).cloned().unwrap();
                if !after.contains(&entry) {
                    after.push(entry);
                }
            }
            if data[0] % 13 == 7 {
                PlanOp::EdgeAdd { step, after }
            } else {
                PlanOp::EdgeRemove { step, after }
            }
        }
        9 if bytes.chance(2) => {
            // a well-formed unit.add: a recipe that parses, its params, sound overrides
            let recipe = (*bytes.pick(&["lane", "pair", "solo"]).unwrap()).to_owned();
            let mut params = JsonMap::default();
            if recipe == "lane" {
                params.0.insert("ticket".into(), json_value(json!("FIG-2")));
            }
            let mut after = IndexMap::new();
            if let Some(step) = bytes.pick(&target.steps)
                && bytes.chance(2)
            {
                after.insert("*".into(), vec![step.clone()]);
            }
            let mut inputs = IndexMap::new();
            if recipe == "lane" && bytes.chance(2) {
                inputs.insert(
                    "work".into(),
                    JsonMap(IndexMap::from([("b".into(), json_value(json!(9)))])),
                );
            }
            PlanOp::UnitAdd {
                recipe,
                unit: format!("w{}", bytes.next() % 3).parse().unwrap(),
                params,
                after,
                inputs,
                tags: if bytes.chance(2) {
                    vec!["wave-2".into()]
                } else {
                    vec![]
                },
            }
        }
        9 => {
            let recipe = (*bytes
                .pick(&[
                    "lane", "lane", "pair", "solo", "outside", "broken", "absent",
                ])
                .unwrap())
            .to_owned();
            let unit = match bytes.next() % 6 {
                0 => target.unit(&mut bytes),
                _ => format!("w{}", bytes.next() % 3),
            };
            let mut params = JsonMap::default();
            match (recipe.as_str(), bytes.next() % 5) {
                ("lane", 0) => {}
                ("lane", 1) => {
                    params.0.insert("ticket".into(), json_value(json!(7)));
                }
                ("lane", _) => {
                    params.0.insert("ticket".into(), json_value(json!("FIG-1")));
                }
                ("pair", 0) => {
                    params.0.insert("n".into(), json_value(json!(3)));
                }
                (_, 4) => {
                    params.0.insert("unit".into(), json_value(json!("other")));
                }
                (_, 3) => {
                    params.0.insert("bogus".into(), json_value(json!(1)));
                }
                _ => {}
            }
            let mut after = IndexMap::new();
            match bytes.next() % 6 {
                0 => {
                    after.insert("*".into(), vec![target.step(&mut bytes)]);
                }
                1 => {
                    after.insert("work".into(), vec![target.step(&mut bytes)]);
                }
                2 => {
                    after.insert("nosuch".into(), vec!["s0".into()]);
                }
                _ => {}
            }
            let mut inputs = IndexMap::new();
            match bytes.next() % 6 {
                0 => {
                    inputs.insert(
                        "work".into(),
                        JsonMap(IndexMap::from([("a".into(), json_value(json!(5)))])),
                    );
                }
                1 => {
                    inputs.insert(
                        "fork".into(),
                        JsonMap(IndexMap::from([("nope".into(), json_value(json!(5)))])),
                    );
                }
                _ => {}
            }
            let tags = match bytes.next() % 6 {
                0 => vec!["wave-2".into()],
                1 => vec!["unit:x".into()],
                _ => vec![],
            };
            PlanOp::UnitAdd {
                recipe,
                unit: unit.parse().unwrap(),
                params,
                after,
                inputs,
                tags,
            }
        }
        10 => {
            let unit = target.unit(&mut bytes);
            let mut changes = IndexMap::new();
            let members = target.units.get(&unit).cloned().unwrap_or_default();
            for _ in 0..1 + bytes.next() % 2 {
                let id = match bytes.pick(&members) {
                    Some(id) if !bytes.chance(8) => id.clone(),
                    _ => target.step(&mut bytes),
                };
                let change = build_changes(target, catalog, &id, &mut bytes);
                changes.insert(StepId::new(&id).unwrap(), change);
            }
            PlanOp::UnitUpdate {
                unit: unit.parse().unwrap(),
                changes,
            }
        }
        11 => PlanOp::UnitRemove {
            unit: target.unit(&mut bytes).parse().unwrap(),
        },
        _ => {
            let (collection, mut ids) = match bytes.next() % 3 {
                0 => (OrderCollection::Steps, target.steps.clone()),
                1 => (OrderCollection::Inputs, target.inputs.clone()),
                _ => (OrderCollection::Outputs, target.outputs.clone()),
            };
            for i in (1..ids.len()).rev() {
                let j = bytes.next() as usize % (i + 1);
                ids.swap(i, j);
            }
            match bytes.next() % 8 {
                0 if !ids.is_empty() => {
                    ids.pop();
                }
                1 if !ids.is_empty() => ids.push(ids[0].clone()),
                2 => ids.push("stranger".into()),
                _ => {}
            }
            PlanOp::OrderSet { collection, ids }
        }
    }
}

/// `step.update`'s changes for `step`: one to three fields, each set to something plausible
/// (a new fn, bindings over the other steps, gates that may close a cycle, a unit move, a
/// pause, …) or removed.
fn build_changes(
    target: &Target,
    catalog: &IndexMap<String, FnSignature>,
    step: &str,
    bytes: &mut Bytes,
) -> StepChanges {
    let scope = target.scope(catalog, step);
    let old = target.declaration(step);
    let run = old
        .get("run")
        .and_then(Value::as_str)
        .filter(|r| catalog.contains_key(*r))
        .unwrap_or("t.flag")
        .to_owned();
    let mut changes = StepChanges::default();
    let some = |v: Value| Some(Some(json_value(v)));
    for _ in 0..1 + bytes.next() % 3 {
        match bytes.next() % 11 {
            0 => {
                let fns: Vec<&String> = catalog.keys().collect();
                let new = (*bytes.pick(&fns).unwrap()).clone();
                changes.run = some(json!(new));
                let fresh = scope.step(&new, None, step, bytes);
                changes.bindings = Some(fresh.get("in").cloned().map(json_value));
            }
            1 => {
                let fresh = scope.step(&run, None, step, bytes);
                changes.bindings = Some(fresh.get("in").cloned().map(json_value));
            }
            2 => {
                let gates = scope.gates("", bytes);
                changes.after = if gates.is_empty() {
                    Some(None)
                } else {
                    some(json!(gates))
                };
            }
            3 => {
                let unit = target.unit(bytes);
                changes.tags = match bytes.next() % 4 {
                    0 => Some(None),
                    1 => some(json!([format!("unit:{unit}")])),
                    2 => some(json!([format!("unit:{unit}"), "exit"])),
                    _ => some(json!(["unit:a", "unit:b"])),
                };
            }
            4 => {
                changes.paused = match bytes.next() % 3 {
                    0 => Some(None),
                    1 => some(json!(true)),
                    _ => some(json!("held")),
                };
            }
            5 => {
                changes.priority = match bytes.next() % 3 {
                    0 => Some(None),
                    1 => some(json!(5)),
                    _ => some(json!("high")),
                };
            }
            6 => {
                changes.needs = match bytes.next() % 4 {
                    0 => Some(None),
                    1 => some(json!({"r1": 1})),
                    2 => some(json!({"r1": 5})),
                    _ => some(json!({"r9": 1})),
                };
            }
            7 => {
                changes.doc = if bytes.chance(2) {
                    Some(None)
                } else {
                    some(json!("Changed"))
                };
            }
            8 => {
                changes.outputs = match bytes.next() % 3 {
                    0 => Some(None),
                    1 => some(json!({"flag":"int"})),
                    _ => some(json!({"extra":"string"})),
                };
            }
            9 => {
                changes.scatter = if bytes.chance(2) {
                    Some(None)
                } else {
                    some(json!("items"))
                };
            }
            _ => {
                let fns: Vec<&String> = catalog.keys().collect();
                changes.run = some(json!(bytes.pick(&fns).unwrap()));
            }
        }
    }
    changes
}

/// An edit case from bytes over a base document: the base's state, then one to `ops.len()`
/// operations each aimed at the candidate the ones before it made (a refused one is kept
/// and aims nothing), `start` and the resources.
pub fn build_case(document: &JsonMap, state: &[u8], ops_data: &[[u8; 12]], misc: u8) -> EditCase {
    let catalog = catalog();
    let recipes = recipes();
    let base = ops::rows_of(document, header(Revision(1))).expect("a generated base");
    let plan = Plan::parse(document, &catalog).expect("a generated base compiles");
    let mut ops = vec![];
    let mut rows = base.clone();
    for data in ops_data {
        let op = build_op(&Target::of(&rows), &catalog, data);
        ops.push(op);
        if let Ok(applied) = ops::apply(&base, &ops, true, &recipes, &catalog) {
            rows = applied.rows;
        }
    }
    let mut resources = CachedResources::default();
    resources.capacities.insert("r1".into(), Some(2));
    resources
        .capacities
        .insert("r2".into(), (misc & 0x10 != 0).then_some(1));
    if misc & 0x20 != 0 {
        resources.leased.insert("r1".into(), 1);
    }
    EditCase {
        base,
        ops,
        start: !misc.is_multiple_of(8),
        state: build_state(&plan, state),
        resources,
        limits: limits(),
    }
}
/// Edit cases over bases of at most `max_steps` steps with one to `max_ops` operations.
pub fn case(max_steps: usize, max_ops: usize) -> impl Strategy<Value = EditCase> {
    (
        plan(max_steps),
        prop::collection::vec(any::<u8>(), 24),
        prop::collection::vec(any::<[u8; 12]>(), 1..=max_ops),
        any::<u8>(),
    )
        .prop_map(|(document, state, ops, misc)| build_case(&document, &state, &ops, misc))
}

/// The reference context of the generated catalog: `catalog()` and `recipes()`.
pub struct Fixture {
    pub signatures: IndexMap<String, FnSignature>,
    pub recipes: IndexMap<String, RecipeEntry>,
}
impl Fixture {
    pub fn new() -> Self {
        Self {
            signatures: catalog(),
            recipes: recipes(),
        }
    }
    pub fn context(&self) -> Context<'_, IndexMap<String, FnSignature>> {
        Context {
            signatures: &self.signatures,
            recipes: &self.recipes,
        }
    }
}
impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

/// A typed edit command (today's wire form, `{"command": …, "args": …}`) aimed at a
/// compiled base, from bytes: `step_add`, `step_update`, `step_remove`, `edge_add`,
/// `edge_remove`, `unit_add`, `step_pause`, `unit_tag`, `step_set_input`, `plan_prune` or
/// `plan_patch` (a few RFC 6902 operations at rev 1).
pub fn build_command(document: &JsonMap, data: &[u8]) -> Value {
    let catalog = catalog();
    let rows = ops::rows_of(document, header(Revision(1))).expect("a generated base");
    let target = Target::of(&rows);
    let mut bytes = Bytes::new(data);
    let edit = json!({"expected": null, "dry_run": false, "reason": "generated", "author": null});
    let project = json!({"kind": "name", "value": "gen"});
    let selection = |bytes: &mut Bytes| {
        if bytes.chance(3) {
            json!({"steps": null, "tags": [bytes.pick(&["wave-1", "arc:x", "none"]).unwrap()]})
        } else {
            json!({"steps": [target.step(bytes)], "tags": null})
        }
    };
    let kind = bytes.next() % 11;
    let (command, args) = match kind {
        10 => {
            let mut ops = vec![];
            for _ in 0..1 + bytes.next() % 3 {
                let step = target.step(&mut bytes);
                let other = target.step(&mut bytes);
                let op = match bytes.next() % 9 {
                    0 => json!({"op":"replace","path":format!("/steps/{step}/priority"),"value":3}),
                    1 => json!({"op":"add","path":format!("/steps/{step}/paused"),"value":true}),
                    2 => json!({"op":"remove","path":format!("/steps/{step}")}),
                    3 => json!({"op":"add","path":"/inputs/extra","value":"int"}),
                    4 => json!({"op":"move","from":format!("/steps/{step}"),"path":"/steps/moved"}),
                    5 => {
                        json!({"op":"copy","from":format!("/steps/{step}"),"path":"/steps/copied"})
                    }
                    6 => json!({"op":"test","path":format!("/steps/{step}/run"),"value":"t.add"}),
                    7 => json!({"op":"add","path":format!("/steps/{step}/after"),"value":[other]}),
                    _ => {
                        json!({"op":"replace","path":format!("/steps/{step}"),"value":{"run":"t.flag"}})
                    }
                };
                ops.push(op);
            }
            return json!({"command": "plan_patch", "args": {
                "project": project, "rev": 1, "ops": ops, "start": !bytes.chance(4),
                "dry_run": false, "reason": "generated", "author": null}});
        }
        0 => {
            let op = build_op(&target, &catalog, &with_kind(data, 4));
            let PlanOp::StepAdd { step, spec } = op else {
                unreachable!()
            };
            (
                "step_add",
                json!({"step": step, "spec": spec, "start": !bytes.chance(4)}),
            )
        }
        1 => {
            let step = target.step(&mut bytes);
            let changes = build_changes(&target, &catalog, &step, &mut bytes);
            ("step_update", json!({"step": step, "changes": changes}))
        }
        2 => ("step_remove", json!({"selection": selection(&mut bytes)})),
        3 | 4 => {
            let op = build_op(&target, &catalog, &with_kind(data, 7));
            let PlanOp::EdgeAdd { step, after } = op else {
                unreachable!()
            };
            let command = if kind == 3 { "edge_add" } else { "edge_remove" };
            (command, json!({"step": step, "after": after}))
        }
        5 => {
            let op = build_op(&target, &catalog, &with_kind(data, 9));
            let PlanOp::UnitAdd {
                recipe,
                unit,
                params,
                after,
                inputs,
                tags,
            } = op
            else {
                unreachable!()
            };
            (
                "unit_add",
                json!({"recipe": recipe, "unit": unit, "params": params, "start": !bytes.chance(4),
                    "after": after, "inputs": inputs, "tags": tags}),
            )
        }
        6 => (
            "step_pause",
            json!({"selection": selection(&mut bytes), "subtree": bytes.chance(2),
                "paused": !bytes.chance(3)}),
        ),
        7 => {
            let unit = target.unit(&mut bytes);
            let add = match bytes.next() % 3 {
                0 => json!(["wave-2"]),
                1 => json!(["unit:x"]),
                _ => json!([]),
            };
            let remove = match bytes.next() % 3 {
                0 => json!(["wave-1"]),
                1 => json!(["wave-2"]),
                _ => json!([]),
            };
            (
                "unit_tag",
                json!({"unit": unit, "add": add, "remove": remove}),
            )
        }
        8 => {
            let names = ["a", "text", "x0", "attempts", "spec"];
            let name = *bytes.pick(&names).unwrap();
            let value = match name {
                "a" => json!(bytes.next() % 3),
                "attempts" => json!([1]),
                _ => json!("set"),
            };
            (
                "step_set_input",
                json!({"selection": selection(&mut bytes), "inputs": {name: value}}),
            )
        }
        _ => {
            let units: Vec<String> = target.units.keys().cloned().collect();
            let chosen = match bytes.next() % 3 {
                0 => json!(null),
                1 => json!([bytes
                    .pick(&units)
                    .cloned()
                    .unwrap_or_else(|| "nowhere".into())]),
                _ => json!(units),
            };
            let keep = if bytes.chance(3) {
                json!(["u*"])
            } else {
                json!(null)
            };
            (
                "plan_prune",
                json!({"units": chosen, "tags": null, "older_than_seconds": 0, "keep": keep}),
            )
        }
    };
    let mut args = args;
    args["project"] = project;
    args["edit"] = edit;
    json!({"command": command, "args": args})
}
/// `data` with its first byte set so `build_op` builds operation kind `kind`.
fn with_kind(data: &[u8], kind: u8) -> Vec<u8> {
    let mut data = data.to_vec();
    if data.is_empty() {
        data.push(0);
    }
    data[0] = kind;
    data
}
