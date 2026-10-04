//! Forgiving tool arguments, shared by MCP, HTTP, a helper's `ctx.tool` and `sluice tool`
//! (SPEC §12.2): an unknown tool or argument is refused with the nearest valid names, and a
//! string of decimal digits is taken where the command's schema wants an integer. Both work
//! from the commands' generated JSON schema, never from a list of names kept by hand.
use serde_json::{Map, Value, json};
use sluice_model::error::PublicError;

fn bad(message: impl Into<String>) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}

/// Levenshtein distance over chars.
pub fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous + usize::from(ca != *cb);
            previous = row[j + 1];
            row[j + 1] = substitute.min(previous + 1).min(row[j] + 1);
        }
    }
    row[b.len()]
}

/// The valid names nearest `given`, nearest first and at most three: those within edit
/// distance 3 (less for a short name, so `to` does not suggest every two-letter field), and
/// those that contain it or that it contains (`message` suggests `to_message`). Case and `-`
/// for `_` are ignored.
pub fn closest<'a>(given: &str, names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let given = given.to_ascii_lowercase().replace('-', "_");
    let limit = (given.chars().count() / 2).clamp(1, 3);
    let mut found: Vec<(usize, &str)> = names
        .into_iter()
        .filter_map(|name| {
            let d = distance(&given, name);
            let inside = given.len() >= 3
                && name.len() >= 3
                && (name.contains(given.as_str()) || given.contains(name));
            (d <= limit || inside).then_some((if inside { d.min(limit) } else { d }, name))
        })
        .collect();
    found.sort();
    found.dedup_by(|a, b| a.1 == b.1);
    found.into_iter().take(3).map(|(_, name)| name).collect()
}

/// `did you mean a?`, `did you mean a or b?`, `did you mean a, b or c?`, or None.
pub fn did_you_mean<'a>(given: &str, names: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let found = closest(given, names);
    let (last, rest) = found.split_last()?;
    Some(if rest.is_empty() {
        format!("did you mean {last}?")
    } else {
        format!("did you mean {} or {last}?", rest.join(", "))
    })
}

/// `unknown tool <name>; did you mean <nearest>?`, else `unknown tool <name>` and `otherwise`.
pub fn unknown_tool<'a>(
    name: &str,
    names: impl IntoIterator<Item = &'a str>,
    otherwise: &str,
) -> PublicError {
    match did_you_mean(name, names) {
        Some(hint) => bad(format!("unknown tool {name}; {hint}")),
        None => bad(format!("unknown tool {name}{otherwise}")),
    }
}

/// `<tool> takes no argument '<key>'; did you mean <nearest>?`, else the arguments it takes.
pub fn unknown_argument<'a>(
    tool: &str,
    key: &str,
    names: impl IntoIterator<Item = &'a str> + Clone,
) -> PublicError {
    match did_you_mean(key, names.clone()) {
        Some(hint) => bad(format!("{tool} takes no argument '{key}'; {hint}")),
        None => bad(format!(
            "{tool} takes no argument '{key}'; its arguments are {}",
            names.into_iter().collect::<Vec<_>>().join(", ")
        )),
    }
}

/// What a schema takes, following `$ref` (into `defs`), `anyOf`/`oneOf` and type lists.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Accepts {
    pub integer: bool,
    pub number: bool,
    pub string: bool,
    pub boolean: bool,
    pub array: bool,
    pub object: bool,
    pub null: bool,
    /// Unconstrained: any JSON value.
    pub any: bool,
}
impl Accepts {
    /// Only strings (or null): a command-line value for it is taken as text, never as JSON.
    pub fn only_string(&self) -> bool {
        self.string
            && !(self.any
                || self.integer
                || self.number
                || self.boolean
                || self.array
                || self.object)
    }
    /// An integer (or number) and no string: a decimal string means that integer.
    pub fn wants_integer(&self) -> bool {
        (self.integer || self.number) && !self.string && !self.any
    }
    /// A short type name for help: `integer`, `string`, `array|string`, `any`, ...
    pub fn name(&self) -> String {
        if self.any {
            return "any".into();
        }
        let names: Vec<&str> = [
            (self.boolean, "boolean"),
            (self.integer, "integer"),
            (self.number && !self.integer, "number"),
            (self.string, "string"),
            (self.array, "array"),
            (self.object, "object"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        match names.len() {
            0 if self.null => "null".into(),
            0 => "any".into(),
            n if n >= 5 => "any".into(),
            _ => names.join("|"),
        }
    }
}

/// The leaf schemas of `schema`: `$ref`s resolved, `anyOf`/`oneOf` branches flattened.
fn leaves<'a>(schema: &'a Value, defs: &'a Value, depth: usize, out: &mut Vec<&'a Value>) {
    if depth > 32 {
        return;
    }
    if let Some(name) = schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix("#/$defs/"))
    {
        if let Some(def) = defs.get(name) {
            leaves(def, defs, depth + 1, out);
        }
        return;
    }
    let mut branched = false;
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = schema.get(key).and_then(Value::as_array) {
            branched = true;
            for branch in branches {
                leaves(branch, defs, depth + 1, out);
            }
        }
    }
    if !branched {
        out.push(schema);
    }
}

/// What `schema` takes; `defs` resolves its `#/$defs/` references.
pub fn accepts(schema: &Value, defs: &Value) -> Accepts {
    let mut found = Vec::new();
    leaves(schema, defs, 0, &mut found);
    let mut accepts = Accepts::default();
    for leaf in found {
        let types: Vec<&str> = match leaf.get("type") {
            Some(Value::String(t)) => vec![t.as_str()],
            Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
            _ => {
                let constant = leaf.get("const").or_else(|| leaf.get("enum"));
                match constant {
                    Some(Value::String(_)) => vec!["string"],
                    Some(Value::Array(values)) if values.iter().all(Value::is_string) => {
                        vec!["string"]
                    }
                    Some(Value::Null) => vec!["null"],
                    _ => {
                        accepts.any = true;
                        continue;
                    }
                }
            }
        };
        for t in types {
            match t {
                "integer" => accepts.integer = true,
                "number" => accepts.number = true,
                "string" => accepts.string = true,
                "boolean" => accepts.boolean = true,
                "array" => accepts.array = true,
                "object" => accepts.object = true,
                "null" => accepts.null = true,
                _ => accepts.any = true,
            }
        }
    }
    accepts
}

/// What the items of an array `schema` take, when it takes an array.
pub fn item_accepts(schema: &Value, defs: &Value) -> Option<Accepts> {
    let mut found = Vec::new();
    leaves(schema, defs, 0, &mut found);
    found
        .iter()
        .find_map(|leaf| leaf.get("items"))
        .map(|items| accepts(items, defs))
}

/// A field's type for help: its string values (`inbox|questions|history|thread`) when it is
/// an enumeration, else `Accepts::name`.
pub fn type_text(schema: &Value, defs: &Value) -> String {
    let mut found = Vec::new();
    leaves(schema, defs, 0, &mut found);
    let mut values = Vec::new();
    for leaf in &found {
        match (leaf.get("enum"), leaf.get("const"), leaf.get("type")) {
            (Some(Value::Array(items)), _, _) => {
                values.extend(items.iter().filter_map(Value::as_str));
            }
            (_, Some(Value::String(value)), _) => values.push(value),
            (_, _, Some(Value::String(t))) if t == "null" => {}
            _ => return accepts(schema, defs).name(),
        }
    }
    if values.is_empty() {
        accepts(schema, defs).name()
    } else {
        values.join("|")
    }
}

/// Takes a string of decimal digits as the integer wherever `schema` wants an integer and no
/// string, at any depth of the arguments (array items, object properties). A string that is
/// not one is refused naming its field. `schema` is a tool's input schema: `properties` and
/// the `$defs` they reference.
pub fn coerce_integers(schema: &Value, args: &mut Map<String, Value>) -> Result<(), PublicError> {
    let defs = &schema["$defs"];
    for (key, value) in args.iter_mut() {
        if let Some(field) = schema["properties"].get(key) {
            coerce(field, defs, value, key)?;
        }
    }
    Ok(())
}

fn coerce(schema: &Value, defs: &Value, value: &mut Value, path: &str) -> Result<(), PublicError> {
    match value {
        Value::String(text) => {
            if accepts(schema, defs).wants_integer() {
                let digits = text.trim();
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(bad(format!("{path}: expected an integer, not {text:?}")));
                }
                let number: u64 = digits
                    .parse()
                    .map_err(|_| bad(format!("{path}: integer out of range: {digits}")))?;
                *value = json!(number);
            }
        }
        Value::Array(items) => {
            let mut found = Vec::new();
            leaves(schema, defs, 0, &mut found);
            if let Some(item) = found.iter().find_map(|leaf| leaf.get("items")) {
                for (i, entry) in items.iter_mut().enumerate() {
                    coerce(item, defs, entry, &format!("{path}[{i}]"))?;
                }
            }
        }
        Value::Object(fields) => {
            let mut found = Vec::new();
            leaves(schema, defs, 0, &mut found);
            if let Some(properties) = found
                .iter()
                .find_map(|leaf| leaf.get("properties").and_then(Value::as_object))
            {
                for (key, entry) in fields.iter_mut() {
                    if let Some(field) = properties.get(key) {
                        coerce(field, defs, entry, &format!("{path}.{key}"))?;
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// A tool description split into its text and its `Args:` block, field by field: the MCP
/// descriptions list each argument as `    name: text` with continuation lines indented
/// further.
pub fn described_args(description: &str) -> (String, Vec<(String, String)>) {
    let mut text = Vec::new();
    let mut args: Vec<(String, String)> = Vec::new();
    let mut in_args = false;
    for line in description.lines() {
        if line.trim() == "Args:" {
            in_args = true;
            continue;
        }
        if in_args {
            if let Some(entry) = line.strip_prefix("    ")
                && !entry.starts_with(' ')
                && let Some((name, rest)) = entry.split_once(": ")
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                args.push((name.to_owned(), rest.trim().to_owned()));
                continue;
            }
            if line.starts_with("        ")
                && let Some((_, rest)) = args.last_mut()
            {
                rest.push(' ');
                rest.push_str(line.trim());
                continue;
            }
            in_args = false;
        }
        if line.trim().is_empty() && text.last().is_some_and(|l: &&str| l.trim().is_empty()) {
            continue;
        }
        text.push(line);
    }
    while text.last().is_some_and(|line| line.trim().is_empty()) {
        text.pop();
    }
    (text.join("\n"), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_names() {
        assert_eq!(distance("step_contxt", "step_context"), 1);
        assert_eq!(
            closest("step_contxt", ["step_context", "step_cancel", "status"]),
            ["step_context"]
        );
        assert_eq!(
            closest("output", ["project", "step", "run", "outputs", "author"]),
            ["outputs"]
        );
        assert_eq!(closest("message", ["to_message", "body"]), ["to_message"]);
        assert_eq!(
            closest("to-message", ["to_message", "body"]),
            ["to_message"]
        );
        assert!(closest("zz", ["to", "ui", "body"]).is_empty());
        assert_eq!(
            did_you_mean("stepz", ["steps", "step", "tags"]).as_deref(),
            Some("did you mean step or steps?")
        );
    }

    #[test]
    fn decimal_strings_become_integers_only_where_the_schema_wants_one() {
        let schema = json!({
            "properties": {
                "id": {"$ref": "#/$defs/MessageId"},
                "since": {"anyOf": [{"$ref": "#/$defs/MessageId"}, {"type": "null"}]},
                "limit": {"type": ["integer", "null"]},
                "name": {"type": "string"},
                "value": {"anyOf": [{"type": "string"}, {"type": "integer"}]},
                "ids": {"type": "array", "items": {"type": "integer"}},
                "nested": {"type": "object", "properties": {"n": {"type": "integer"}}},
            },
            "$defs": {"MessageId": {"type": "integer", "format": "int64"}},
        });
        let mut args = json!({"id": "24771", "since": " 3 ", "limit": "10", "name": "7",
            "value": "8", "ids": ["1", 2], "nested": {"n": "4"}});
        coerce_integers(&schema, args.as_object_mut().unwrap()).unwrap();
        assert_eq!(
            args,
            json!({"id": 24771, "since": 3, "limit": 10, "name": "7", "value": "8",
                "ids": [1, 2], "nested": {"n": 4}})
        );
        for (key, value) in [("id", "abc"), ("limit", "-1"), ("ids", "x")] {
            let mut args = Map::new();
            let value = if key == "ids" {
                json!([value])
            } else {
                json!(value)
            };
            args.insert(key.into(), value);
            let Err(PublicError::BadRequest { message }) = coerce_integers(&schema, &mut args)
            else {
                panic!("{key}")
            };
            assert!(message.starts_with(key), "{message}");
        }
    }

    #[test]
    fn args_blocks_split_field_by_field() {
        let (text, args) = described_args(
            "Reply.\n\nArgs:\n    project: the project.\n    body: the reply,\n        markdown.\n\ndry_run: more.",
        );
        assert_eq!(text, "Reply.\n\ndry_run: more.");
        assert_eq!(
            args,
            [
                ("project".to_owned(), "the project.".to_owned()),
                ("body".to_owned(), "the reply, markdown.".to_owned())
            ]
        );
    }
}
