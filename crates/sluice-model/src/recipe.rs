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
    /// The `title` template as written, and why it does not check (it is then not used).
    title: Option<String>,
    title_error: Option<String>,
    /// The unit `view` as written, its checked root, and why it does not check.
    view: Option<String>,
    view_root: Option<crate::openui::Component>,
    view_error: Option<String>,
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
                "expected an object {name, doc?, params?, steps, title?, view?}",
            )]);
        };
        for key in raw
            .keys()
            .filter(|k| !["name", "doc", "params", "steps", "title", "view"].contains(&k.as_str()))
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
        let text = |key: &str, errors: &mut Vec<PathError>| match raw.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => {
                errors.push(diagnostic(key, "expected a string"));
                None
            }
        };
        let title = text("title", &mut errors);
        let view = text("view", &mut errors);
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut recipe = Self {
            name: name.into(),
            doc,
            params,
            steps,
            title,
            title_error: None,
            view,
            view_root: None,
            view_error: None,
        };
        // a title or view that does not check never breaks the recipe: units are still
        // added from it, and the dashboard names and draws them as if it had none
        recipe.title_error = recipe
            .title
            .as_deref()
            .and_then(|title| recipe.check_title(title).err());
        if let Some(view) = recipe.view.clone() {
            match recipe.check_view(&view) {
                Ok(root) => recipe.view_root = Some(root),
                Err(problems) => recipe.view_error = Some(problems),
            }
        }
        Ok(recipe)
    }

    /// The `title` template as written (`recipe_list` shows it), checked or not.
    pub fn title_source(&self) -> Option<&str> {
        self.title.as_deref()
    }
    /// The `title` template when it checks.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref().filter(|_| self.title_error.is_none())
    }
    /// Why the `title` does not check.
    pub fn title_error(&self) -> Option<&str> {
        self.title_error.as_deref()
    }
    /// The unit `view` as written, checked or not.
    pub fn view_source(&self) -> Option<&str> {
        self.view.as_deref()
    }
    /// The unit view's checked root (`openui::VIEW_COMPONENTS`).
    pub fn view(&self) -> Option<&crate::openui::Component> {
        self.view_root.as_ref()
    }
    /// Why the `view` does not check: each problem with its line, "; "-joined.
    pub fn view_error(&self) -> Option<&str> {
        self.view_error.as_deref()
    }
    /// The recipe's stages, in its steps' order: each step key's id without the leading
    /// `{unit}-` ("fork", "work"), the key itself when it does not start so. A key that uses
    /// another param has no fixed stage and is left out.
    pub fn stages(&self) -> Vec<String> {
        let mut out = vec![];
        for key in self.steps.as_object().into_iter().flat_map(Map::keys) {
            let Some(id) = only_unit(key, "\u{0}") else {
                continue;
            };
            out.push(
                id.strip_prefix("\u{0}-")
                    .map(str::to_owned)
                    .unwrap_or_else(|| id.replace('\u{0}', "")),
            );
        }
        out
    }
    /// The params some step uses (`unit` always): what a unit's steps can give back.
    pub fn used_params(&self) -> IndexSet<String> {
        let mut used = IndexSet::from(["unit".to_owned()]);
        fn strings(text: &str, used: &mut IndexSet<String>) {
            for token in tokens(text, "", &mut vec![]) {
                if let Token::Param(name) = token {
                    used.insert(name.to_owned());
                }
            }
        }
        fn walk(value: &Value, used: &mut IndexSet<String>) {
            match value {
                Value::String(text) => strings(text, used),
                Value::Array(items) => items.iter().for_each(|v| walk(v, used)),
                Value::Object(fields) => {
                    for (key, value) in fields {
                        strings(key, used);
                        walk(value, used);
                    }
                }
                _ => {}
            }
        }
        walk(&self.steps, &mut used);
        used
    }
    /// The params a step binds as a file (`{"file": "{spec}"}`): in a title or view each
    /// stands for that file's title, never its path.
    pub fn file_params(&self) -> IndexSet<String> {
        let mut out = IndexSet::new();
        fn walk(value: &Value, out: &mut IndexSet<String>) {
            match value {
                Value::Object(fields) => {
                    if fields.len() == 1
                        && let Some(Value::String(text)) = fields.get("file")
                        && let [Token::Param(name)] = tokens(text, "", &mut vec![]).as_slice()
                    {
                        out.insert((*name).to_owned());
                    }
                    fields.values().for_each(|v| walk(v, out));
                }
                Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
                _ => {}
            }
        }
        walk(&self.steps, &mut out);
        out
    }
    /// `text` with each `{param}` replaced by `values[param]` (`{{`/`}}` literal braces); a
    /// param without a value reads as nothing.
    pub fn fill(text: &str, values: &IndexMap<String, String>) -> String {
        tokens(text, "", &mut vec![])
            .into_iter()
            .map(|token| match token {
                Token::Text(text) => text.to_owned(),
                Token::Param(name) => values.get(name).cloned().unwrap_or_default(),
            })
            .collect()
    }
    /// Whether `steps` (a unit's step ids and stored declarations) are this recipe's steps for
    /// unit `unit`, and if so the params read back from them. They are when expanding each step
    /// key with `{unit}` gives exactly the unit's ids and each `run` is the recipe's. A param is
    /// read where a step holds it: a string that is exactly `{p}` gives the stored value as it
    /// is; text around placeholders is matched and each takes what lies between. The first
    /// reading of a param wins. A param no step uses is not read.
    pub fn match_unit(
        &self,
        unit: &str,
        steps: &Map<String, Value>,
    ) -> Option<IndexMap<String, Value>> {
        let templates = self.steps.as_object()?;
        if templates.len() != steps.len() {
            return None;
        }
        let mut bound = IndexMap::from([("unit".to_owned(), Value::String(unit.to_owned()))]);
        for (key, template) in templates {
            let stored = steps.get(&only_unit(key, unit)?)?;
            let run = |v: &Value| v.get("run").and_then(Value::as_str).map(str::to_owned);
            if let (Some(want), Some(have)) = (run(template), run(stored))
                && let Some(want) = only_unit(&want, unit)
                && want != have
            {
                return None;
            }
            unify(template, stored, &mut bound);
        }
        Some(bound)
    }
    fn check_title(&self, title: &str) -> Result<(), String> {
        let mut errors = vec![];
        let used = self.used_params();
        for token in tokens(title, "title", &mut errors) {
            if let Token::Param(name) = token {
                if !self.params.contains_key(name) {
                    errors.push(diagnostic("title", format!("unknown param {{{name}}}")));
                } else if !used.contains(name) {
                    errors.push(diagnostic(
                        "title",
                        format!(
                            "{{{name}}} is a param no step uses, so a unit's steps cannot give it back"
                        ),
                    ));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "))
        }
    }
    fn check_view(&self, view: &str) -> Result<crate::openui::Component, String> {
        use crate::openui::{Problem, Value as Ui};
        let joined = |problems: Vec<Problem>| {
            problems
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        };
        let (root, mut problems) = crate::openui::check_view_parts(view);
        let Some(root) = root else {
            problems.sort();
            return Err(joined(problems));
        };
        let (stages, used) = (self.stages(), self.used_params());
        for c in crate::openui::components(&root) {
            let mut fail = |message: String| {
                problems.push(Problem {
                    line: c.line,
                    message,
                })
            };
            match c.name.as_str() {
                "Output" | "StepStatus" => {
                    if let Some(step) = c.str_arg(0)
                        && !stages.iter().any(|s| s == step)
                    {
                        fail(format!(
                            "{}: step is one of the recipe's stages ({}), not {step:?}",
                            c.name,
                            stages.join(", ")
                        ));
                    }
                }
                "Param" => {
                    if let Some(name) = c.str_arg(0) {
                        if !self.params.contains_key(name) {
                            fail(format!("Param: the recipe has no param {name}"));
                        } else if !used.contains(name) {
                            fail(format!(
                                "Param: {name} is a param no step uses, so a unit's steps cannot give it back"
                            ));
                        }
                    }
                }
                _ => {}
            }
            for arg in &c.args {
                let Ui::String(text) = arg else { continue };
                let mut errors = vec![];
                for token in tokens(text, "view", &mut errors) {
                    if let Token::Param(name) = token
                        && !(self.params.contains_key(name) && used.contains(name))
                    {
                        fail(format!(
                            "{{{name}}} is not a param a unit's steps give back"
                        ));
                    }
                }
                for error in errors {
                    fail(error.message);
                }
            }
        }
        problems.sort();
        problems.dedup();
        if problems.is_empty() {
            Ok(root)
        } else {
            Err(joined(problems))
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

/// `text` with `{unit}` replaced by `unit`; `None` when it uses another param.
fn only_unit(text: &str, unit: &str) -> Option<String> {
    let mut out = String::new();
    for token in tokens(text, "", &mut vec![]) {
        match token {
            Token::Text(text) => out.push_str(text),
            Token::Param("unit") => out.push_str(unit),
            Token::Param(_) => return None,
        }
    }
    Some(out)
}
/// Read params back from `stored`, where `template` holds them (`Recipe::match_unit`).
fn unify(template: &Value, stored: &Value, bound: &mut IndexMap<String, Value>) {
    match (template, stored) {
        (Value::String(text), _) => {
            let tokens = tokens(text, "", &mut vec![]);
            if let [Token::Param(name)] = tokens.as_slice() {
                bound
                    .entry((*name).to_owned())
                    .or_insert_with(|| stored.clone());
                return;
            }
            let Value::String(stored) = stored else {
                return;
            };
            // literal parts must match; each placeholder takes what lies before the next one
            let mut at = 0;
            let mut found = vec![];
            for (i, token) in tokens.iter().enumerate() {
                match token {
                    Token::Text(literal) => {
                        if !stored[at..].starts_with(literal) {
                            return;
                        }
                        at += literal.len();
                    }
                    Token::Param(name) => {
                        let end = match tokens.get(i + 1) {
                            Some(Token::Text(next)) => match stored[at..].find(next) {
                                Some(offset) => at + offset,
                                None => return,
                            },
                            Some(Token::Param(_)) => return, // two in a row: ambiguous
                            None => stored.len(),
                        };
                        found.push((*name, stored[at..end].to_owned()));
                        at = end;
                    }
                }
            }
            if at != stored.len() {
                return;
            }
            for (name, value) in found {
                bound.entry(name.to_owned()).or_insert(Value::String(value));
            }
        }
        (Value::Object(template), Value::Object(stored)) => {
            for (key, value) in template {
                let key = Recipe::fill(
                    key,
                    &bound
                        .iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                        .collect(),
                );
                if let Some(stored) = stored.get(&key) {
                    unify(value, stored, bound);
                }
            }
        }
        (Value::Array(template), Value::Array(stored)) => {
            for (value, stored) in template.iter().zip(stored) {
                unify(value, stored, bound);
            }
        }
        _ => {}
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
