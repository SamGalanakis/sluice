use crate::{
    ids::{StepId, UnitName},
    rpc::JsonValue,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "of",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Type {
    String,
    Int,
    Float,
    Boolean,
    Any,
    Optional(Box<Type>),
    List(Box<Type>),
    Enum(Vec<String>),
    Record(
        #[schemars(with = "std::collections::BTreeMap<String, Type>")]
        indexmap::IndexMap<String, Type>,
    ),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum BoundValue {
    Ready(JsonValue),
    Waiting,
    Skipped(SkipReason),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SkipReason {
    Boolean {
        reference: String,
        value: Option<bool>,
        negate: bool,
    },
    Step {
        step: StepId,
    },
    Unit {
        unit: UnitName,
        exit: StepId,
    },
}

/// A diagnostic with a field/index path. Paths use `field.child[0]` spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathError {
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.path.is_empty() {
            write!(f, "{}: ", self.path)?;
        }
        f.write_str(&self.message)
    }
}
impl std::error::Error for PathError {}

fn error(path: &str, message: impl Into<String>) -> PathError {
    PathError {
        path: path.into(),
        message: message.into(),
    }
}
fn field_path(path: &str, field: &str) -> String {
    if path.is_empty() {
        field.into()
    } else {
        format!("{path}.{field}")
    }
}

/// The shared strict decoder rejects duplicate keys, overflowing integer tokens,
/// non-finite numbers, trailing data and documents larger than 16 MiB.
pub use crate::rpc::decode_json;

impl Type {
    /// Parse a SPEC §3 type expression from strict JSON bytes.
    /// Serde's tagged representation remains the shared RPC contract; use this
    /// method and `form` for type expressions in fn and plan declarations.
    pub fn parse_json(bytes: &[u8]) -> Result<Self, PathError> {
        let value: JsonValue = decode_json(bytes).map_err(|e| error("type", e.to_string()))?;
        Self::parse(value.as_value())
    }

    /// Parse a decoded SPEC expression. Duplicate keys must be rejected during
    /// decoding, before constructing a `Value` loses their occurrence information.
    pub fn parse(form: &serde_json::Value) -> Result<Self, PathError> {
        validate_json(form, "type").map_err(|mut errors| errors.remove(0))?;
        parse_type(form, "type", 0)
    }

    /// Write the SPEC expression, using suffix shorthand wherever possible.
    pub fn form(&self) -> serde_json::Value {
        use serde_json::{Value, json};
        match self {
            Self::String => json!("string"),
            Self::Int => json!("int"),
            Self::Float => json!("float"),
            Self::Boolean => json!("boolean"),
            Self::Any => json!("Any"),
            Self::Optional(inner) => match inner.form() {
                Value::String(s) => json!(format!("{s}?")),
                other => json!(["null", other]),
            },
            Self::List(inner) => match inner.form() {
                Value::String(s) => json!(format!("{s}[]")),
                other => json!({"type":"array", "items":other}),
            },
            Self::Enum(symbols) => json!({"type":"enum", "symbols":symbols}),
            Self::Record(fields) => json!({"type":"record", "fields":fields.iter()
                .map(|(name, t)| (name.clone(), t.form())).collect::<serde_json::Map<_, _>>()}),
        }
    }
}

impl std::fmt::Display for Type {
    /// Display a complete JSON type expression, including quotes on primitives.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.form(), f)
    }
}
impl std::str::FromStr for Type {
    type Err = PathError;
    /// Accept a JSON expression or a bare primitive/suffix expression.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.starts_with(['"', '[', '{']) {
            Self::parse_json(s.as_bytes())
        } else {
            Self::parse(&serde_json::Value::String(s.into()))
        }
    }
}

fn parse_type(form: &serde_json::Value, path: &str, depth: usize) -> Result<Type, PathError> {
    use serde_json::Value;
    if depth >= 128 {
        return Err(error(path, "type nesting exceeds 128"));
    }
    match form {
        Value::String(s) => {
            if let Some(inner) = s.strip_suffix('?') {
                return Ok(Type::Optional(Box::new(parse_type(
                    &Value::String(inner.into()),
                    path,
                    depth + 1,
                )?)));
            }
            if let Some(inner) = s.strip_suffix("[]") {
                return Ok(Type::List(Box::new(parse_type(
                    &Value::String(inner.into()),
                    path,
                    depth + 1,
                )?)));
            }
            match s.as_str() {
                "string" => Ok(Type::String),
                "int" => Ok(Type::Int),
                "float" => Ok(Type::Float),
                "boolean" => Ok(Type::Boolean),
                "Any" => Ok(Type::Any),
                _ => Err(error(path, format!("unknown type {s:?}"))),
            }
        }
        Value::Array(a) if a.len() == 2 => {
            let inner = if a[0] == "null" {
                &a[1]
            } else if a[1] == "null" {
                &a[0]
            } else {
                return Err(error(path, "expected a two-element null union"));
            };
            Ok(Type::Optional(Box::new(parse_type(
                inner,
                path,
                depth + 1,
            )?)))
        }
        Value::Object(o) if o.len() == 2 => match o.get("type").and_then(Value::as_str) {
            Some("array") if o.contains_key("items") => Ok(Type::List(Box::new(parse_type(
                &o["items"],
                &field_path(path, "items"),
                depth + 1,
            )?))),
            Some("enum") if o.contains_key("symbols") => {
                let symbols = o["symbols"]
                    .as_array()
                    .and_then(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>());
                match symbols {
                    Some(symbols)
                        if !symbols.is_empty()
                            && symbols
                                .iter()
                                .collect::<std::collections::HashSet<_>>()
                                .len()
                                == symbols.len() =>
                    {
                        Ok(Type::Enum(symbols.into_iter().map(str::to_owned).collect()))
                    }
                    _ => Err(error(
                        &field_path(path, "symbols"),
                        "a non-empty list of distinct strings",
                    )),
                }
            }
            Some("record") if o.contains_key("fields") => {
                let fields = o["fields"]
                    .as_object()
                    .ok_or_else(|| error(&field_path(path, "fields"), "expected an object"))?;
                Ok(Type::Record(
                    fields
                        .iter()
                        .map(|(name, form)| {
                            Ok((
                                name.clone(),
                                parse_type(form, &field_path(path, name), depth + 1)?,
                            ))
                        })
                        .collect::<Result<_, PathError>>()?,
                ))
            }
            _ => Err(error(path, "not a type expression")),
        },
        _ => Err(error(path, "not a type expression")),
    }
}

/// Structural compatibility, including Any in either direction and record width.
pub fn fits(source: &Type, target: &Type) -> bool {
    use Type::*;
    match (source, target) {
        (Any, _) | (_, Any) => true,
        (Optional(s), Optional(t)) => fits(s, t),
        (s, Optional(t)) => fits(s, t),
        (Optional(_), _) => false,
        (Int, Float) => true,
        (Enum(_), String) => true,
        (Enum(s), Enum(t)) => s.iter().all(|symbol| t.contains(symbol)),
        (List(s), List(t)) => fits(s, t),
        (Record(s), Record(t)) => t.iter().all(|(name, target)| match s.get(name) {
            Some(source) => fits(source, target),
            None => matches!(target, Optional(_)),
        }),
        _ => source == target,
    }
}

/// Validate JSON numbers throughout the value, including extra record fields.
pub(crate) fn validate_json(value: &serde_json::Value, path: &str) -> Result<(), Vec<PathError>> {
    fn visit(value: &serde_json::Value, path: &str, depth: usize, errors: &mut Vec<PathError>) {
        use serde_json::Value;
        if depth >= 128 {
            errors.push(error(path, "JSON nesting exceeds 128"));
            return;
        }
        match value {
            Value::Number(n)
                if !(if n.is_f64() {
                    n.as_f64().is_some_and(f64::is_finite)
                } else {
                    n.as_i64().is_some()
                }) =>
            {
                errors.push(error(
                    path,
                    "JSON integers must fit i64 and floats must be finite",
                ))
            }
            Value::Array(a) => {
                for (i, v) in a.iter().enumerate() {
                    visit(v, &format!("{path}[{i}]"), depth + 1, errors);
                }
            }
            Value::Object(o) => {
                for (k, v) in o {
                    visit(v, &field_path(path, k), depth + 1, errors);
                }
            }
            _ => {}
        }
    }
    let mut errors = Vec::new();
    visit(value, path, 0, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub fn check_value(t: &Type, value: &serde_json::Value) -> Result<(), Vec<PathError>> {
    check_value_at(t, value, "")
}

/// Validate a runtime value with a diagnostic prefix such as `steps.a.in.n`.
pub fn check_value_at(
    t: &Type,
    value: &serde_json::Value,
    path: &str,
) -> Result<(), Vec<PathError>> {
    validate_json(value, path)?;
    fn check(t: &Type, v: &serde_json::Value, p: &str, errors: &mut Vec<PathError>) {
        use Type::*;
        let expected = match t {
            Any => return,
            Optional(_) if v.is_null() => return,
            Optional(inner) => {
                check(inner, v, p, errors);
                return;
            }
            String if v.is_string() => return,
            Int if v.as_i64().is_some() => return,
            Float if v.is_number() => return,
            Boolean if v.is_boolean() => return,
            Enum(symbols)
                if v.as_str()
                    .is_some_and(|s| symbols.iter().any(|symbol| symbol == s)) =>
            {
                return;
            }
            List(inner) if v.is_array() => {
                for (i, item) in v.as_array().expect("array checked").iter().enumerate() {
                    check(inner, item, &format!("{p}[{i}]"), errors);
                }
                return;
            }
            Record(fields) if v.is_object() => {
                for (name, t) in fields {
                    let path = field_path(p, name);
                    if let Some(item) = v.get(name) {
                        check(t, item, &path, errors);
                    } else if !matches!(t, Optional(_)) {
                        errors.push(error(&path, "missing required field"));
                    }
                }
                return;
            }
            String => "string".into(),
            Int => "int".into(),
            Float => "float".into(),
            Boolean => "boolean".into(),
            Enum(symbols) => format!("one of [{}]", symbols.join(", ")),
            List(_) => "an array".into(),
            Record(_) => "an object".into(),
        };
        errors.push(error(p, format!("expected {expected}, got {v}")));
    }
    let mut errors = Vec::new();
    check(t, value, path, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Navigate only the field suffix of a reference: for `a/out.field.0`, pass
/// `field.0`. The caller resolves `a/out`. List indices are ASCII decimal.
pub fn navigate(t: &Type, path: &str) -> Result<Type, PathError> {
    if path.is_empty() {
        return Ok(t.clone());
    }
    navigate_segments(t, &path.split('.').collect::<Vec<_>>())
}

/// Segment navigation also supports record keys containing dots.
pub fn navigate_segments(t: &Type, segments: &[&str]) -> Result<Type, PathError> {
    let mut current = t;
    let mut optional = false;
    let mut path = String::new();
    for segment in segments {
        path = field_path(&path, segment);
        while let Type::Optional(inner) = current {
            optional = true;
            current = inner;
        }
        current = match current {
            Type::Any => return Ok(Type::Any),
            Type::Record(fields) => fields
                .get(*segment)
                .ok_or_else(|| error(&path, format!("cannot read {segment} of {current}")))?,
            Type::List(inner) if decimal_index(segment) => inner,
            _ => return Err(error(&path, format!("cannot read {segment} of {current}"))),
        };
    }
    Ok(if optional && !matches!(current, Type::Optional(_)) {
        Type::Optional(Box::new(current.clone()))
    } else {
        current.clone()
    })
}
fn decimal_index(segment: &str) -> bool {
    !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit())
}

/// A missing field/index or a null ancestor returns None. A selected JSON null
/// returns Some(Null), so availability remains distinct from the value itself.
pub fn navigate_value<'a>(
    mut value: &'a serde_json::Value,
    segments: &[&str],
) -> Option<&'a serde_json::Value> {
    for segment in segments {
        value = match value {
            serde_json::Value::Object(fields) => fields.get(*segment)?,
            serde_json::Value::Array(items) if decimal_index(segment) => {
                items.get(segment.parse::<usize>().ok()?)?
            }
            _ => return None,
        };
    }
    Some(value)
}
