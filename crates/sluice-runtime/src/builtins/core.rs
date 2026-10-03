//! core.* builtins: echo, collect and format run inline; core.external never runs.
//!
//! The shared BuiltinDescriptor/BuiltinCtx/FnFailure contract lives in builtins/jev.rs
//! until p4-02 unifies it; this module reuses it unchanged. `core.external` has no
//! dispatch: a step running it stays pending until `step_set_output` settles it or a
//! pending cancellation fails it at once (both store-side), and the calls path refuses
//! `fn_call` on `is_external` names before admission.

use super::jev::{BuiltinCtx, BuiltinDescriptor, FnFailure};
use indexmap::IndexMap;
use serde_json::Value;
use sluice_model::{
    rpc::{JsonMap, JsonValue},
    types::Type,
};

/// The open builtin standing for work done outside sluice. The scheduler never starts a
/// step running it and `fn_call` refuses it; `gates::readiness` names its wait reason.
pub const EXTERNAL: &str = "core.external";

/// Whether `name` is the external builtin. The calls path refuses fn_call on this name;
/// the scheduler's inline hook must never dispatch it.
pub fn is_external(name: &str) -> bool {
    name == EXTERNAL
}

/// Descriptors equal to the core.* fn.json manifests, in manifest field order.
/// `core.external` declares no ports of its own: it is open, so the plan declares the
/// outputs it will get and binds extra inputs to order it after them.
pub fn descriptors() -> [BuiltinDescriptor; 4] {
    [
        BuiltinDescriptor {
            name: "core.echo",
            doc: "Pass the value through. Built in: runs inline, no process.",
            inputs: ports(&[("value", "Any")]),
            outputs: ports(&[("value", "Any")]),
        },
        BuiltinDescriptor {
            name: "core.collect",
            doc: "The fan-in join: gather values into one array. Built in: runs inline, no \
process.",
            inputs: ports(&[("items", "Any[]")]),
            outputs: ports(&[("items", "Any[]")]),
        },
        BuiltinDescriptor {
            name: "core.format",
            doc: "Fill a template: {name} placeholders draw from a record, {0}, {1}... from \
an array or a single value, and {{ and }} escape the braces. Non-strings render as \
canonical JSON. Built in: runs inline, no process.",
            inputs: ports(&[("template", "string"), ("values", "Any")]),
            outputs: ports(&[("text", "string")]),
        },
        BuiltinDescriptor {
            name: EXTERNAL,
            doc: "Work done outside sluice. The runner never starts it: once ready it waits \
until its outputs are set by hand with step_set_output, or it is cancelled. Declare the \
outputs it will get; bind extra inputs to order it after them.",
            inputs: vec![],
            outputs: vec![],
        },
    ]
}

/// Dispatch for the three inline core fns. `core.external` is refused here too, but the
/// scheduler and calls path must turn it away on `is_external` before dispatch exists.
pub async fn dispatch(
    name: &str,
    inputs: &JsonMap,
    _ctx: &BuiltinCtx,
) -> Result<JsonMap, FnFailure> {
    match name {
        "core.echo" => echo(inputs),
        "core.collect" => collect(inputs),
        "core.format" => format(inputs),
        EXTERNAL => Err(terminal(format!(
            "{EXTERNAL} is work done outside sluice and never runs"
        ))),
        _ => Err(terminal(format!("unknown core builtin: {name}"))),
    }
}

fn echo(inputs: &JsonMap) -> Result<JsonMap, FnFailure> {
    let mut out = JsonMap::default();
    out.0
        .insert("value".into(), required(inputs, "value")?.clone());
    Ok(out)
}

fn collect(inputs: &JsonMap) -> Result<JsonMap, FnFailure> {
    let mut out = JsonMap::default();
    out.0
        .insert("items".into(), required(inputs, "items")?.clone());
    Ok(out)
}

fn format(inputs: &JsonMap) -> Result<JsonMap, FnFailure> {
    let template = required(inputs, "template")?
        .as_value()
        .as_str()
        .ok_or_else(|| terminal("input \"template\" must be a string"))?;
    let values = required(inputs, "values")?.as_value();
    output([("text", Value::String(render(template, values)?))])
}

/// The strict template language: `{{` and `}}` escape the braces, `{name}` fills from a
/// record and `{0}`-style positions fill from an array or a single value. Attribute/item
/// access, conversion flags, format specifications and automatic numbering are refused.
fn render(template: &str, values: &Value) -> Result<String, FnFailure> {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' if bytes.get(i + 1) == Some(&b'{') => {
                out.push('{');
                i += 2;
            }
            b'{' => {
                let rest = &template[i + 1..];
                let Some(end) = rest.find(['}', '{']) else {
                    return Err(terminal(format!(
                        "unclosed '{{' at byte {i}: close it or escape it as '{{{{'"
                    )));
                };
                if rest.as_bytes()[end] == b'{' {
                    return Err(terminal(format!(
                        "unclosed '{{' at byte {i}: close it or escape it as '{{{{'"
                    )));
                }
                out.push_str(&fill(&rest[..end], values)?);
                i += end + 2;
            }
            b'}' if bytes.get(i + 1) == Some(&b'}') => {
                out.push('}');
                i += 2;
            }
            b'}' => {
                return Err(terminal(format!(
                    "unmatched '}}' at byte {i}: escape it as '}}}}'"
                )));
            }
            _ => {
                let rest = &template[i..];
                let stop = rest.find(['{', '}']).unwrap_or(rest.len());
                out.push_str(&rest[..stop]);
                i += stop;
            }
        }
    }
    Ok(out)
}

/// Fill one placeholder, already stripped of its braces. Only a bare name or a bare
/// position is legal here; everything richer is refused with the offending marker.
fn fill(field: &str, values: &Value) -> Result<String, FnFailure> {
    if field.is_empty() {
        return Err(terminal(
            "empty placeholder {{}}: write a name or a position, like {{name}} or {{0}}",
        ));
    }
    for c in field.chars() {
        let refused = match c {
            '!' => "conversion flags",
            ':' => "format specifications",
            '.' | '[' | ']' => "attribute or item access",
            _ => continue,
        };
        return Err(terminal(format!(
            "{refused} are refused: {{{field}}} has '{c}'; \
write a bare name or position, like {{{{name}}}} or {{{{0}}}}"
        )));
    }
    if field.bytes().all(|b| b.is_ascii_digit()) {
        let index = field
            .parse::<usize>()
            .map_err(|_| terminal(format!("placeholder {{{field}}} is out of range")))?;
        positional(field, index, values)
    } else {
        named(field, values)
    }
}

fn positional(field: &str, index: usize, values: &Value) -> Result<String, FnFailure> {
    match values {
        Value::Array(items) => items.get(index).map(show).ok_or_else(|| {
            terminal(format!(
                "placeholder {{{field}}}: the array has {} item(s)",
                items.len()
            ))
        }),
        Value::Object(_) => Err(terminal(format!(
            "placeholder {{{field}}} is a position, but values is a record: \
use {{name}} placeholders"
        ))),
        _ if index == 0 => Ok(show(values)),
        _ => Err(terminal(format!(
            "placeholder {{{field}}}: values is a single value, only {{0}} fills it"
        ))),
    }
}

fn named(field: &str, values: &Value) -> Result<String, FnFailure> {
    match values {
        Value::Object(record) => record
            .get(field)
            .map(show)
            .ok_or_else(|| terminal(format!("the record has no value '{field}'"))),
        Value::Array(_) => Err(terminal(format!(
            "placeholder {{{field}}} is a name, but values is an array: \
use {{0}}-style positions"
        ))),
        _ => Err(terminal(format!(
            "placeholder {{{field}}} is a name, but values is not a record"
        ))),
    }
}

/// Strings render as themselves; every other value renders as canonical JSON: compact,
/// in stored order, under the strict integer/float rules the model enforces.
fn show(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).expect("a strict JSON value serializes"),
    }
}

fn required<'a>(inputs: &'a JsonMap, name: &str) -> Result<&'a JsonValue, FnFailure> {
    inputs
        .0
        .get(name)
        .ok_or_else(|| terminal(format!("missing required input \"{name}\"")))
}
fn output<const N: usize>(pairs: [(&str, Value); N]) -> Result<JsonMap, FnFailure> {
    let mut map = IndexMap::with_capacity(N);
    for (name, value) in pairs {
        map.insert(
            name.to_string(),
            JsonValue::try_from(value).map_err(|e| terminal(e.to_string()))?,
        );
    }
    Ok(JsonMap(map))
}
fn terminal(message: impl Into<String>) -> FnFailure {
    FnFailure::Terminal(message.into())
}

/// The fn.json type grammar these fns declare: names, `?` optional and `[]` list suffixes.
fn ty(form: &str) -> Type {
    if let Some(inner) = form.strip_suffix('?') {
        return Type::Optional(Box::new(ty(inner)));
    }
    if let Some(inner) = form.strip_suffix("[]") {
        return Type::List(Box::new(ty(inner)));
    }
    match form {
        "string" => Type::String,
        "int" => Type::Int,
        "float" => Type::Float,
        "boolean" => Type::Boolean,
        "Any" => Type::Any,
        other => panic!("unknown fn type {other:?}"),
    }
}
fn ports(forms: &[(&'static str, &'static str)]) -> Vec<(&'static str, Type)> {
    forms.iter().map(|(name, form)| (*name, ty(form))).collect()
}
