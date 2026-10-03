//! jev.* builtins: TypeSafe's System One ("Jev") over HTTP, ported from packs/jev.
//!
//! POST {TYPESAFE_BASE_URL or https://api.typesafe.ai}/v1/systemone with a bearer key from
//! TYPESAFE_API_KEY, read from the run's environment. The descriptor, context and failure
//! shapes are p4-02's shared builtin contract, re-exported for this module's callers.

use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use sluice_model::{
    rpc::{JsonMap, JsonValue, decode_json},
    types::Type,
};
use std::time::Duration;

pub use super::descriptor::{BuiltinCtx, BuiltinDescriptor, FnFailure, RetryBudget};

const DEFAULT_BASE: &str = "https://api.typesafe.ai";
const DEFAULT_MODEL: &str = "jev-latest";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const DETAIL_LIMIT: usize = 2000;
/// The pack's `run(main, retries=3, backoff=5)` same-run retry budget.
const JEV_RETRY: RetryBudget = RetryBudget {
    retries: 3,
    backoff: Duration::from_secs(5),
};

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

/// Descriptors equal to packs/jev/*/fn.json, in the same field order.
pub fn descriptors() -> [BuiltinDescriptor; 4] {
    [
        BuiltinDescriptor {
            name: "jev.ask",
            doc: "Ask Jev (TypeSafe System One) several typed questions about one state in a \
single call. questions maps your ids to {type: choice|score|noul, instructions, criteria}; \
answers come back under the same ids. Needs TYPESAFE_API_KEY.",
            inputs: ports(&[("state", "Any"), ("questions", "Any"), ("model", "string?")]),
            outputs: ports(&[("answers", "Any"), ("model", "string"), ("usage", "Any")]),
            open: false,
            submits: vec![],
            icon: None,
            retry: JEV_RETRY,
        },
        BuiltinDescriptor {
            name: "jev.choice",
            doc: "Pick one option for a state with Jev. options is a list of option names or a \
map of option to description. confident is confidence >= min_confidence (default 0.8). \
Needs TYPESAFE_API_KEY.",
            inputs: ports(&[
                ("state", "Any"),
                ("instructions", "Any"),
                ("options", "Any"),
                ("min_confidence", "float?"),
                ("model", "string?"),
            ]),
            outputs: ports(&[
                ("choice", "string"),
                ("probabilities", "Any"),
                ("confidence", "float"),
                ("confident", "boolean"),
                ("model", "string"),
            ]),
            open: false,
            submits: vec![],
            icon: None,
            retry: JEV_RETRY,
        },
        BuiltinDescriptor {
            name: "jev.score",
            doc: "Rate a state on ordered levels (2 to 10, lowest first) with Jev. score is \
the probability-weighted level index and can land between levels. Needs TYPESAFE_API_KEY.",
            inputs: ports(&[
                ("state", "Any"),
                ("instructions", "Any"),
                ("levels", "Any[]"),
                ("model", "string?"),
            ]),
            outputs: ports(&[
                ("score", "float"),
                ("probabilities", "Any"),
                ("confidence", "float"),
                ("legend", "Any"),
                ("model", "string"),
            ]),
            open: false,
            submits: vec![],
            icon: None,
            retry: JEV_RETRY,
        },
        BuiltinDescriptor {
            name: "jev.noul",
            doc: "Probability (0 to 1) that the answer to a yes/no question about a state is \
yes, from Jev. yes/no optionally describe what each answer means. Needs TYPESAFE_API_KEY.",
            inputs: ports(&[
                ("state", "Any"),
                ("instructions", "Any"),
                ("yes", "Any?"),
                ("no", "Any?"),
                ("model", "string?"),
            ]),
            outputs: ports(&[("noul", "float"), ("model", "string")]),
            open: false,
            submits: vec![],
            icon: None,
            retry: JEV_RETRY,
        },
    ]
}

pub async fn dispatch(
    name: &str,
    inputs: &JsonMap,
    ctx: &BuiltinCtx,
) -> Result<JsonMap, FnFailure> {
    match name {
        "jev.ask" => ask(inputs, ctx).await,
        "jev.choice" => choice(inputs, ctx).await,
        "jev.score" => score(inputs, ctx).await,
        "jev.noul" => noul(inputs, ctx).await,
        _ => Err(FnFailure::terminal(format!("unknown jev builtin: {name}"))),
    }
}

/// jev.ask: several typed questions about one state in a single call.
pub async fn ask(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let resp = call(
        ctx,
        req(inputs, "state")?,
        req(inputs, "questions")?,
        opt(inputs, "model"),
    )
    .await?;
    output([
        ("answers", field(&resp, "answers")?.clone()),
        (
            "model",
            resp.get("model").cloned().unwrap_or_else(empty_string),
        ),
        (
            "usage",
            resp.get("usage").cloned().unwrap_or_else(|| json!({})),
        ),
    ])
}

/// jev.choice: pick one option for a state, with probabilities and confidence.
pub async fn choice(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let options = req(inputs, "options")?;
    let criteria = if let Value::Array(items) = options {
        let mut map = Map::new();
        for item in items {
            map.insert(option_key(item)?, Value::Null);
        }
        Value::Object(map)
    } else {
        options.clone()
    };
    if !matches!(&criteria, Value::Object(m) if !m.is_empty()) {
        return Err(FnFailure::terminal(
            "options must be a non-empty list of names or map of name to description",
        ));
    }
    let question = json!({
        "type": "choice",
        "instructions": req(inputs, "instructions")?,
        "criteria": criteria,
    });
    let (answer, model) = one(ctx, req(inputs, "state")?, question, opt(inputs, "model")).await?;
    let choice = field(&answer, "choice")?;
    let probabilities = field(&answer, "probabilities")?;
    let confidence = field(&answer, "confidence")?;
    let fallback = json!(0.8);
    let threshold = match opt(inputs, "min_confidence") {
        Some(v) if !v.is_null() => v,
        _ => &fallback,
    };
    let (given, wanted) = num(confidence)
        .zip(num(threshold))
        .ok_or_else(|| FnFailure::terminal("confidence and min_confidence must be numbers"))?;
    output([
        ("choice", choice.clone()),
        ("probabilities", probabilities.clone()),
        ("confidence", confidence.clone()),
        ("confident", Value::Bool(given >= wanted)),
        ("model", model),
    ])
}

/// jev.score: rate a state on ordered levels, lowest first.
pub async fn score(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let levels = req(inputs, "levels")?;
    let len = py_len(levels).ok_or_else(|| {
        FnFailure::terminal(format!("object of type '{}' has no len()", py_type(levels)))
    })?;
    if !(2..=10).contains(&len) {
        return Err(FnFailure::terminal(format!(
            "levels must have 2 to 10 entries, got {len}"
        )));
    }
    let question = json!({
        "type": "score",
        "instructions": req(inputs, "instructions")?,
        "criteria": levels,
    });
    let (answer, model) = one(ctx, req(inputs, "state")?, question, opt(inputs, "model")).await?;
    output([
        ("score", field(&answer, "score")?.clone()),
        ("probabilities", field(&answer, "probabilities")?.clone()),
        ("confidence", field(&answer, "confidence")?.clone()),
        ("legend", field(&answer, "legend")?.clone()),
        ("model", model),
    ])
}

/// jev.noul: probability that the answer to a yes/no question is yes.
pub async fn noul(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let mut question = Map::new();
    question.insert("type".into(), json!("noul"));
    question.insert("instructions".into(), req(inputs, "instructions")?.clone());
    let mut criteria = Map::new();
    for (key, input) in [("true", "yes"), ("false", "no")] {
        if let Some(value) = opt(inputs, input)
            && !value.is_null()
        {
            criteria.insert(key.into(), value.clone());
        }
    }
    if !criteria.is_empty() {
        question.insert("criteria".into(), Value::Object(criteria));
    }
    let (answer, model) = one(
        ctx,
        req(inputs, "state")?,
        Value::Object(question),
        opt(inputs, "model"),
    )
    .await?;
    output([("noul", field(&answer, "noul")?.clone()), ("model", model)])
}

/// Ask a single question; return (answer, model that answered), like _jev.client.one.
async fn one(
    ctx: &BuiltinCtx,
    state: &Value,
    question: Value,
    model: Option<&Value>,
) -> Result<(Value, Value), FnFailure> {
    let resp = call(ctx, state, &json!({"q": question}), model).await?;
    let answer = resp
        .get("answers")
        .and_then(|answers| answers.get("q"))
        .cloned()
        .ok_or_else(|| FnFailure::terminal("response is missing the \"q\" answer"))?;
    Ok((
        answer,
        resp.get("model").cloned().unwrap_or_else(empty_string),
    ))
}

/// POST {base}/v1/systemone and return the decoded body, like _jev.client.ask.
async fn call(
    ctx: &BuiltinCtx,
    state: &Value,
    questions: &Value,
    model: Option<&Value>,
) -> Result<Value, FnFailure> {
    let key = ctx
        .env("TYPESAFE_API_KEY")
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            FnFailure::terminal(
                "TYPESAFE_API_KEY is not set: put it in the environment of sluice (e.g. \
$SLUICE_HOME/.env)",
            )
        })?;
    let base = ctx
        .env("TYPESAFE_BASE_URL")
        .unwrap_or(DEFAULT_BASE)
        .trim_end_matches('/');
    let model = match model.filter(|m| truthy(m)) {
        Some(m) => m.clone(),
        None => Value::String(
            ctx.env("TYPESAFE_MODEL")
                .unwrap_or(DEFAULT_MODEL)
                .to_string(),
        ),
    };
    let body = json!({"state": state, "model": model, "questions": questions});
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .no_proxy()
        .build()
        .map_err(|e| FnFailure::terminal(format!("typesafe client: {e}")))?;
    let resp = client
        .post(format!("{base}/v1/systemone"))
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .map_err(classify)?;
    let status = resp.status().as_u16();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| FnFailure::terminal(format!("typesafe {status}: unreadable body: {e}")))?;
    if !(200..300).contains(&status) {
        let detail: String = String::from_utf8_lossy(&bytes)
            .chars()
            .take(DETAIL_LIMIT)
            .collect();
        let message = format!("typesafe {status}: {detail}");
        return Err(if status == 429 || status >= 500 {
            FnFailure::Transient(message)
        } else {
            FnFailure::Terminal(message)
        });
    }
    decode_json(&bytes).map_err(|e| FnFailure::terminal(e.to_string()))
}

fn classify(error: reqwest::Error) -> FnFailure {
    if error.is_timeout() {
        FnFailure::Transient(format!("typesafe timed out: {error}"))
    } else if error.is_connect() {
        FnFailure::Transient(format!("typesafe unreachable: {error}"))
    } else {
        FnFailure::terminal(format!("typesafe request failed: {error}"))
    }
}

fn opt<'a>(inputs: &'a JsonMap, name: &str) -> Option<&'a Value> {
    inputs.0.get(name).map(JsonValue::as_value)
}
fn req<'a>(inputs: &'a JsonMap, name: &str) -> Result<&'a Value, FnFailure> {
    opt(inputs, name).ok_or_else(|| FnFailure::terminal(format!("missing required input {name:?}")))
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, FnFailure> {
    value
        .get(name)
        .ok_or_else(|| FnFailure::terminal(format!("response is missing {name:?}")))
}
fn output<const N: usize>(pairs: [(&str, Value); N]) -> Result<JsonMap, FnFailure> {
    let mut map = IndexMap::with_capacity(N);
    for (name, value) in pairs {
        map.insert(
            name.to_string(),
            JsonValue::try_from(value).map_err(|e| FnFailure::terminal(e.to_string()))?,
        );
    }
    Ok(JsonMap(map))
}
fn empty_string() -> Value {
    Value::String(String::new())
}

/// Python truthiness for a JSON value, for `model or TYPESAFE_MODEL or default`.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Numeric coercion matching Python comparisons: bool counts as 0/1.
fn num(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Python len(): strings count characters; numbers and booleans have no len.
fn py_len(value: &Value) -> Option<usize> {
    match value {
        Value::String(s) => Some(s.chars().count()),
        Value::Array(a) => Some(a.len()),
        Value::Object(o) => Some(o.len()),
        _ => None,
    }
}
fn py_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// A list option's criteria key, as Python's dict + json.dumps produce it.
fn option_key(item: &Value) -> Result<String, FnFailure> {
    match item {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Null => Ok("null".into()),
        _ => Err(FnFailure::terminal("options entries must be option names")),
    }
}
