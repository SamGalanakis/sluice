//! Pure recipe loading and expansion. The caller owns file discovery and scope order.

use crate::{
    ids::{StepId, UnitName},
    plan::{Declaration, Plan, SignatureProvider, declaration, diagnostic},
    rpc::{JsonMap, JsonValue, decode_json},
    types::{PathError, Type, check_value_at},
};
use indexmap::{IndexMap, IndexSet};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    name: String,
    doc: String,
    params: IndexMap<String, Declaration>,
    steps: Value,
}

/// A broken higher-precedence file still shadows its lower-precedence namesake.
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeEntry {
    pub name: String,
    pub scope: String,
    pub recipe: Result<Recipe, Vec<PathError>>,
}

/// Sources must be supplied in lookup order, global before project.
pub fn catalog<'a>(
    sources: impl IntoIterator<Item = (&'a str, &'a str, &'a [u8])>,
) -> IndexMap<String, RecipeEntry> {
    let mut entries = IndexMap::new();
    for (name, scope, bytes) in sources {
        entries.insert(
            name.into(),
            RecipeEntry {
                name: name.into(),
                scope: scope.into(),
                recipe: Recipe::parse_json(name, bytes),
            },
        );
    }
    entries
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExpansionOptions {
    pub start: bool,
    pub tags: Vec<String>,
    pub after: IndexMap<String, Vec<String>>,
    pub inputs: IndexMap<String, JsonMap>,
}
impl From<&crate::commands::UnitAdd> for ExpansionOptions {
    fn from(request: &crate::commands::UnitAdd) -> Self {
        Self {
            start: request.start,
            tags: request.tags.clone(),
            after: request.after.clone(),
            inputs: request.inputs.clone(),
        }
    }
}
impl Default for ExpansionOptions {
    fn default() -> Self {
        Self {
            start: true,
            tags: vec![],
            after: IndexMap::new(),
            inputs: IndexMap::new(),
        }
    }
}

impl Recipe {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn doc(&self) -> &str {
        &self.doc
    }
    pub fn params(&self) -> &IndexMap<String, Declaration> {
        &self.params
    }

    pub fn parse_json(name: &str, bytes: &[u8]) -> Result<Self, Vec<PathError>> {
        let raw: JsonValue = decode_json(bytes)
            .map_err(|error| vec![diagnostic("recipe", format!("bad JSON: {error}"))])?;
        Self::parse(name, raw.as_value())
    }

    pub fn parse(name: &str, raw: &Value) -> Result<Self, Vec<PathError>> {
        crate::types::validate_json(raw, "recipe")?;
        let mut errors = vec![];
        let Some(raw) = raw.as_object() else {
            return Err(vec![diagnostic(
                "recipe",
                "expected an object {name, doc?, params?, steps}",
            )]);
        };
        for key in raw
            .keys()
            .filter(|k| !["name", "doc", "params", "steps"].contains(&k.as_str()))
        {
            errors.push(diagnostic(key, "unknown key"));
        }
        if StepId::new(name).is_err() {
            errors.push(diagnostic("name", "invalid recipe name"));
        }
        if raw.get("name").and_then(Value::as_str) != Some(name) {
            errors.push(diagnostic(
                "name",
                format!("must be the file's name, {name}"),
            ));
        }
        let doc = match raw.get("doc") {
            None => String::new(),
            Some(Value::String(doc)) => doc.clone(),
            _ => {
                errors.push(diagnostic("doc", "expected a string"));
                String::new()
            }
        };
        let mut params = IndexMap::from([(
            "unit".into(),
            Declaration {
                ty: Type::String,
                doc: None,
            },
        )]);
        if let Some(raw_params) = raw.get("params") {
            if let Some(raw_params) = raw_params.as_object() {
                for (key, form) in raw_params {
                    let path = format!("params.{key}");
                    if StepId::new(key).is_err() {
                        errors.push(diagnostic(&path, "invalid param name"));
                    }
                    if let Some(decl) = declaration(form, &path, &mut errors) {
                        if key == "unit" && decl.ty != Type::String {
                            errors.push(diagnostic(&path, "the unit is always a string"));
                        } else {
                            params.insert(key.clone(), decl);
                        }
                    }
                }
            } else {
                errors.push(diagnostic("params", "expected an object of name -> type"));
            }
        }
        let steps = raw.get("steps").cloned().unwrap_or(Value::Null);
        if let Some(steps) = steps.as_object().filter(|steps| !steps.is_empty()) {
            for (id, step) in steps {
                match step.as_object() {
                    Some(step) if step.contains_key("when") => errors.push(diagnostic(
                        &format!("steps.{id}.when"),
                        "when is removed; use after entries",
                    )),
                    None => {
                        errors.push(diagnostic(&format!("steps.{id}"), "expected a step object"))
                    }
                    _ => {}
                }
            }
            check_placeholders(&Value::Object(steps.clone()), &params, "steps", &mut errors);
        } else {
            errors.push(diagnostic(
                "steps",
                "required, a nonempty object of step id -> step",
            ));
        }
        if errors.is_empty() {
            Ok(Self {
                name: name.into(),
                doc,
                params,
                steps,
            })
        } else {
            Err(errors)
        }
    }

    /// Substitute params only. Staging and whole-plan validation belong to `expand`.
    pub fn substitute(&self, params: &JsonMap) -> Result<JsonMap, Vec<PathError>> {
        let mut errors = vec![];
        for key in params
            .0
            .keys()
            .filter(|key| !self.params.contains_key(*key))
        {
            errors.push(diagnostic(
                &format!("params.{key}"),
                format!("recipe {} has no param {key}", self.name),
            ));
        }
        let mut values = IndexMap::new();
        for (name, decl) in &self.params {
            let path = format!("params.{name}");
            let value = match params.0.get(name) {
                Some(value) => value.as_value().clone(),
                None if matches!(decl.ty, Type::Optional(_)) => Value::Null,
                None => {
                    errors.push(diagnostic(&path, "required"));
                    continue;
                }
            };
            if name == "unit" && value.as_str().is_none_or(|s| UnitName::new(s).is_err()) {
                errors.push(diagnostic(&path, "expected a valid unit name"));
            } else if let Err(found) = check_value_at(&decl.ty, &value, &path) {
                errors.extend(found);
            }
            values.insert(name.clone(), value);
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let steps = substitute_value(&self.steps, &values, "steps", &mut errors);
        if !errors.is_empty() {
            return Err(errors);
        }
        decode_json(&serde_json::to_vec(&steps).expect("JSON value serializes"))
            .map_err(|error| vec![diagnostic("steps", error.to_string())])
    }

    /// Entries are derived by the plan core before any external staging gates are appended.
    /// The supplied document supplies external references. No file or fn is executed.
    pub fn expand(
        &self,
        params: &JsonMap,
        options: &ExpansionOptions,
        document: &JsonMap,
        signatures: &impl SignatureProvider,
    ) -> Result<JsonMap, Vec<PathError>> {
        let steps = self.substitute(params)?;
        let unit = params.0["unit"].as_value().as_str().expect("checked unit");
        let mut errors = reserved_tags(&options.tags, "tags");
        let mut steps: Map<String, Value> = steps
            .0
            .into_iter()
            .map(|(k, v)| (k, v.into_value()))
            .collect();
        let mut by = IndexMap::new();
        for (id, step) in &mut steps {
            if StepId::new(id).is_err() {
                errors.push(diagnostic(&format!("steps.{id}"), "invalid step id"));
            }
            let suffix = id
                .strip_prefix(&format!("{unit}-"))
                .unwrap_or(id)
                .to_owned();
            if by.insert(suffix.clone(), id.clone()).is_some() {
                errors.push(diagnostic(
                    &format!("steps.{id}"),
                    format!("ambiguous suffix {suffix}"),
                ));
            }
            let Some(step) = step.as_object_mut() else {
                errors.push(diagnostic(&format!("steps.{id}"), "expected a step object"));
                continue;
            };
            let own = match step.get("tags") {
                Some(Value::Array(tags)) if tags.iter().all(Value::is_string) => tags
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned())
                    .collect(),
                None => vec![],
                _ => {
                    errors.push(diagnostic(
                        &format!("steps.{id}.tags"),
                        "expected an array of tags",
                    ));
                    vec![]
                }
            };
            errors.extend(reserved_tags(&own, &format!("steps.{id}.tags")));
            let tags: IndexSet<_> = std::iter::once(format!("unit:{unit}"))
                .chain(own)
                .chain(options.tags.iter().cloned())
                .collect();
            step.insert("tags".into(), serde_json::json!(tags));
            if !options.start && !step.contains_key("paused") {
                step.insert("paused".into(), Value::Bool(true));
            }
        }
        for suffix in options
            .after
            .keys()
            .filter(|s| s.as_str() != "*" && !by.contains_key(*s))
        {
            errors.push(diagnostic(
                &format!("after.{suffix}"),
                format!("recipe {} has no step {suffix}", self.name),
            ));
        }
        for (suffix, inputs) in &options.inputs {
            let Some(id) = by.get(suffix) else {
                errors.push(diagnostic(
                    &format!("inputs.{suffix}"),
                    format!("recipe {} has no step {suffix}", self.name),
                ));
                continue;
            };
            let Some(step) = steps[id].as_object_mut() else {
                continue;
            };
            let signature = step
                .get("run")
                .and_then(Value::as_str)
                .and_then(|name| signatures.signature(name));
            for (name, value) in &inputs.0 {
                let bound = step
                    .get("in")
                    .and_then(Value::as_object)
                    .is_some_and(|inputs| inputs.contains_key(name));
                if !bound
                    && !signature
                        .as_ref()
                        .is_some_and(|s| s.inputs.contains_key(name))
                {
                    errors.push(diagnostic(
                        &format!("inputs.{suffix}.{name}"),
                        format!("step {id} has no input {name}"),
                    ));
                    continue;
                }
                if let Some(bindings) = step
                    .entry("in")
                    .or_insert_with(|| serde_json::json!({}))
                    .as_object_mut()
                {
                    bindings.insert(name.clone(), serde_json::json!({"default":value}));
                } else {
                    errors.push(diagnostic(&format!("steps.{id}.in"), "expected an object"));
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let staged = combined(document, &steps)?;
        let compiled = Plan::parse(&staged, signatures)?;
        let entries = &compiled.units()[&UnitName::new(unit).expect("checked unit")].entries;
        for (suffix, gates) in &options.after {
            let targets = if suffix == "*" {
                entries
                    .iter()
                    .filter(|id| steps.contains_key(id.as_str()))
                    .map(ToString::to_string)
                    .collect()
            } else {
                vec![by[suffix].clone()]
            };
            for id in targets {
                let step = steps[&id].as_object_mut().expect("checked step");
                let old = step
                    .get("after")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|v| v.as_str().expect("validated gate").to_owned());
                let gates: IndexSet<_> = old.chain(gates.iter().cloned()).collect();
                step.insert("after".into(), serde_json::json!(gates));
            }
        }
        Plan::parse(&combined(document, &steps)?, signatures)?;
        Ok(JsonMap(
            steps
                .into_iter()
                .map(|(k, v)| (k, JsonValue::try_from(v).expect("checked JSON")))
                .collect(),
        ))
    }
}

fn combined(document: &JsonMap, steps: &Map<String, Value>) -> Result<JsonMap, Vec<PathError>> {
    let mut document = serde_json::to_value(document).expect("JSON map serializes");
    let Some(existing) = document.get_mut("steps").and_then(Value::as_object_mut) else {
        return Err(vec![diagnostic("steps", "expected an object")]);
    };
    let errors: Vec<_> = steps
        .keys()
        .filter(|id| existing.contains_key(*id))
        .map(|id| diagnostic(&format!("steps.{id}"), "already exists in the plan"))
        .collect();
    if !errors.is_empty() {
        return Err(errors);
    }
    existing.extend(steps.clone());
    serde_json::from_value(document).map_err(|error| vec![diagnostic("plan", error.to_string())])
}

pub(crate) fn reserved_tags(tags: &[String], path: &str) -> Vec<PathError> {
    tags.iter()
        .filter(|tag| tag.starts_with("unit:"))
        .map(|tag| {
            diagnostic(
                path,
                format!("{tag} is reserved (a unit's steps carry unit:<unit>)"),
            )
        })
        .collect()
}

fn check_placeholders(
    value: &Value,
    params: &IndexMap<String, Declaration>,
    path: &str,
    errors: &mut Vec<PathError>,
) {
    match value {
        Value::String(text) => {
            for token in tokens(text, path, errors) {
                if let Token::Param(name) = token
                    && !params.contains_key(name)
                {
                    errors.push(diagnostic(path, format!("unknown param {{{name}}}")));
                }
            }
        }
        Value::Array(values) => {
            for (i, value) in values.iter().enumerate() {
                check_placeholders(value, params, &format!("{path}[{i}]"), errors);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                check_placeholders(
                    &Value::String(key.clone()),
                    params,
                    &format!("{path}.{key} (key)"),
                    errors,
                );
                check_placeholders(value, params, &format!("{path}.{key}"), errors);
            }
        }
        _ => {}
    }
}

#[derive(Debug)]
enum Token<'a> {
    Text(&'a str),
    Param(&'a str),
}
fn tokens<'a>(text: &'a str, path: &str, errors: &mut Vec<PathError>) -> Vec<Token<'a>> {
    let mut out = vec![];
    let mut at = 0;
    while at < text.len() {
        let tail = &text[at..];
        let Some(offset) = tail.find(['{', '}']) else {
            out.push(Token::Text(tail));
            break;
        };
        if offset > 0 {
            out.push(Token::Text(&tail[..offset]));
            at += offset;
            continue;
        }
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push(Token::Text(&tail[..1]));
            at += 2;
            continue;
        }
        if tail.starts_with('{')
            && let Some(end) = tail[1..].find(['{', '}']).map(|n| n + 1)
            && tail.as_bytes()[end] == b'}'
        {
            out.push(Token::Param(&tail[1..end]));
            at += end + 1;
            continue;
        }
        errors.push(diagnostic(
            path,
            format!("a lone '{}'; double it for a literal brace", &tail[..1]),
        ));
        at += 1;
    }
    out
}
fn substitute_value(
    value: &Value,
    params: &IndexMap<String, Value>,
    path: &str,
    errors: &mut Vec<PathError>,
) -> Value {
    match value {
        Value::String(text) => {
            let tokens = tokens(text, path, errors);
            if let [Token::Param(name)] = tokens.as_slice()
                && let Some(value) = params.get(*name)
            {
                return value.clone();
            }
            let mut result = String::new();
            for token in tokens {
                match token {
                    Token::Text(text) => result.push_str(text),
                    Token::Param(name) => match params.get(name) {
                        Some(Value::String(text)) => result.push_str(text),
                        Some(value) => result.push_str(&value.to_string()),
                        None => errors.push(diagnostic(path, format!("unknown param {{{name}}}"))),
                    },
                }
            }
            Value::String(result)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .enumerate()
                .map(|(i, v)| substitute_value(v, params, &format!("{path}[{i}]"), errors))
                .collect(),
        ),
        Value::Object(values) => {
            let mut result = Map::new();
            for (key, value) in values {
                let key = substitute_value(
                    &Value::String(key.clone()),
                    params,
                    &format!("{path}.{key} (key)"),
                    errors,
                );
                let key = key
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| key.to_string());
                let value = substitute_value(value, params, &format!("{path}.{key}"), errors);
                if result.contains_key(&key) {
                    errors.push(diagnostic(
                        &format!("{path}.{key}"),
                        "duplicate key after substitution",
                    ));
                }
                result.insert(key, value);
            }
            Value::Object(result)
        }
        _ => value.clone(),
    }
}
