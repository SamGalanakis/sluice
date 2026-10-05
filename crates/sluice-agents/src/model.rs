//! The agent fns' `model` input: a JSON object naming what an engine runs. A request parses
//! it (refusing the retired string forms and the separate `effort` input with the object to
//! use instead); the supervisor composes it into the id the engine takes and checks that id
//! against what the engine lists before the agent starts. An unknown id fails the run; nothing
//! else is run in its place.
use crate::engines::{EngineError, EngineErrorKind, claude, codex, devin};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, LazyLock, Mutex, PoisonError},
    time::{Duration, Instant},
};

/// How long an engine's model listing is reused within one process.
pub const CATALOG_TTL: Duration = Duration::from_secs(600);
/// The most nearest ids an unknown-model error names.
pub const NEAREST: usize = 5;
/// Effort words the retired string forms ended with (`swe-2-high`), for the hint only.
const EFFORT_WORDS: [&str; 8] = [
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

/// What an agent fn runs, a strict union of two objects (unknown keys refused, strings
/// non-empty, `fast` and `priority` booleans):
/// `{"type":"normal","model":M,"effort":E?,"fast":bool?}` on any engine, and (Devin only)
/// `{"type":"fusion","main":{"model":M,"effort":E?,"fast":bool?},
/// "sidekick":{"model":S,"effort":F?,"priority":bool?}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Value", into = "Value")]
pub enum ModelChoice {
    Normal(Main),
    Fusion { main: Main, sidekick: Sidekick },
}
/// A normal model, or a fusion's main model: `M[-E][-fast]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Main {
    pub model: String,
    pub effort: Option<String>,
    pub fast: bool,
}
/// A fusion's sidekick: `S[-F][-priority]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidekick {
    pub model: String,
    pub effort: Option<String>,
    pub priority: bool,
}
impl Main {
    /// `M[-E][-fast]`, as Devin names it.
    pub fn id(&self) -> String {
        joined(
            &self.model,
            self.effort.as_deref(),
            self.fast.then_some("fast"),
        )
    }
}
impl Sidekick {
    /// `S[-F][-priority]`, as Devin names it.
    pub fn id(&self) -> String {
        joined(
            &self.model,
            self.effort.as_deref(),
            self.priority.then_some("priority"),
        )
    }
}
fn joined(model: &str, effort: Option<&str>, flag: Option<&str>) -> String {
    [Some(model), effort, flag]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("-")
}

impl ModelChoice {
    pub fn normal(model: &str, effort: Option<&str>) -> Self {
        Self::Normal(Main {
            model: model.into(),
            effort: effort.map(str::to_owned),
            fast: false,
        })
    }
    pub fn to_value(&self) -> Value {
        Value::from(self.clone())
    }
    /// The object as one line of JSON, as errors and docs show it.
    pub fn json(&self) -> String {
        self.to_value().to_string()
    }
    /// Reads the object strictly: a known `type`, its fields with their types and no others.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "model must be a JSON object".to_owned())?;
        match object.get("type") {
            Some(Value::String(kind)) if kind == "normal" => {
                only(object, "model", &["type", "model", "effort", "fast"])?;
                Ok(Self::Normal(Main {
                    model: text(object, "model", "model.model")?,
                    effort: optional_text(object, "effort", "model.effort")?,
                    fast: flag(object, "fast", "model.fast")?,
                }))
            }
            Some(Value::String(kind)) if kind == "fusion" => {
                only(object, "model", &["type", "main", "sidekick"])?;
                let main = record(object, "main", "model.main")?;
                only(main, "model.main", &["model", "effort", "fast"])?;
                let sidekick = record(object, "sidekick", "model.sidekick")?;
                only(sidekick, "model.sidekick", &["model", "effort", "priority"])?;
                Ok(Self::Fusion {
                    main: Main {
                        model: text(main, "model", "model.main.model")?,
                        effort: optional_text(main, "effort", "model.main.effort")?,
                        fast: flag(main, "fast", "model.main.fast")?,
                    },
                    sidekick: Sidekick {
                        model: text(sidekick, "model", "model.sidekick.model")?,
                        effort: optional_text(sidekick, "effort", "model.sidekick.effort")?,
                        priority: flag(sidekick, "priority", "model.sidekick.priority")?,
                    },
                })
            }
            _ => Err(r#"model.type must be "normal" or "fusion""#.into()),
        }
    }
}
impl TryFrom<Value> for ModelChoice {
    type Error = String;
    fn try_from(value: Value) -> Result<Self, String> {
        Self::parse(&value)
    }
}
impl From<ModelChoice> for Value {
    fn from(choice: ModelChoice) -> Value {
        fn part(model: &str, effort: Option<&str>, flag: (&str, bool)) -> Map<String, Value> {
            let mut object = Map::new();
            object.insert("model".into(), model.into());
            if let Some(effort) = effort {
                object.insert("effort".into(), effort.into());
            }
            if flag.1 {
                object.insert(flag.0.into(), true.into());
            }
            object
        }
        match choice {
            ModelChoice::Normal(main) => {
                let mut object = Map::new();
                object.insert("type".into(), "normal".into());
                object.extend(part(
                    &main.model,
                    main.effort.as_deref(),
                    ("fast", main.fast),
                ));
                Value::Object(object)
            }
            ModelChoice::Fusion { main, sidekick } => {
                let mut object = Map::new();
                object.insert("type".into(), "fusion".into());
                object.insert(
                    "main".into(),
                    Value::Object(part(
                        &main.model,
                        main.effort.as_deref(),
                        ("fast", main.fast),
                    )),
                );
                object.insert(
                    "sidekick".into(),
                    Value::Object(part(
                        &sidekick.model,
                        sidekick.effort.as_deref(),
                        ("priority", sidekick.priority),
                    )),
                );
                Value::Object(object)
            }
        }
    }
}
fn only(object: &Map<String, Value>, at: &str, allowed: &[&str]) -> Result<(), String> {
    match object.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(key) => Err(format!(
            "{at} has no field {key:?} (it takes {})",
            allowed.join(", ")
        )),
        None => Ok(()),
    }
}
fn record<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    at: &str,
) -> Result<&'a Map<String, Value>, String> {
    object
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{at} must be an object"))
}
fn text(object: &Map<String, Value>, key: &str, at: &str) -> Result<String, String> {
    optional_text(object, key, at)?.ok_or_else(|| format!("{at} is required"))
}
fn optional_text(
    object: &Map<String, Value>,
    key: &str,
    at: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if !s.is_empty() && s.trim() == s => Ok(Some(s.clone())),
        Some(_) => Err(format!("{at} must be a non-empty string")),
    }
}
fn flag(object: &Map<String, Value>, key: &str, at: &str) -> Result<bool, String> {
    match object.get(key) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{at} must be true or false")),
    }
}

/// The model an engine runs when the `model` input is left out: the object a caller could pass.
pub fn default_for(engine: &str) -> Option<ModelChoice> {
    match engine {
        "devin" => devin::profile::profile().default_model,
        "codex" => codex::profile::profile().default_model,
        "claude" => claude::profile::profile().default_model,
        _ => crate::ScriptedEngine::new(vec![]).profile.default_model,
    }
}

/// What a launch runs: `id` names it (Devin's model id; `<model>@<effort>` for Codex and
/// Claude, which take the two apart), as the catalog lists it and the run records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedModel {
    pub id: String,
    pub model: String,
    pub effort: Option<String>,
}

/// The launch's model: the supervisor's checked one, else (an adapter driven directly) the
/// engine default, composed.
pub fn launch_model(
    engine: &str,
    model: Option<&ResolvedModel>,
) -> Result<ResolvedModel, EngineError> {
    if let Some(model) = model {
        return Ok(model.clone());
    }
    default_for(engine)
        .ok_or_else(|| format!("{engine} has no default model"))
        .and_then(|choice| compose(engine, &choice))
        .map_err(|message| EngineError {
            kind: EngineErrorKind::CapabilityMismatch,
            message,
            retry_at: None,
        })
}

/// Composes the id the engine takes. Devin: `M[-E][-fast]`, and for a fusion
/// `fusion-M[-E][-fast]-sidekick-S[-F][-priority]`. Codex and Claude take the model (Codex's
/// `sol` and `astra` name its pinned models) and the effort apart, recorded as
/// `<model>@<effort>` (`<model>` alone without an effort, which runs the CLI's own default);
/// neither can run fast, and fusion runs only on Devin: those are refused.
pub fn compose(engine: &str, choice: &ModelChoice) -> Result<ResolvedModel, String> {
    match (engine, choice) {
        ("devin" | "fake", ModelChoice::Normal(main)) => Ok(whole(main.id())),
        ("devin", ModelChoice::Fusion { main, sidekick }) => Ok(whole(format!(
            "fusion-{}-sidekick-{}",
            main.id(),
            sidekick.id()
        ))),
        ("codex" | "claude", ModelChoice::Normal(main)) => {
            if main.fast {
                return Err(format!("{engine} cannot run a model fast; drop model.fast"));
            }
            let model = if engine == "codex" {
                codex::profile::model_name(&main.model)
            } else {
                &main.model
            };
            Ok(ResolvedModel {
                id: match &main.effort {
                    Some(effort) => format!("{model}@{effort}"),
                    None => model.into(),
                },
                model: model.into(),
                effort: main.effort.clone(),
            })
        }
        (_, ModelChoice::Fusion { .. }) => Err(format!(
            "fusion runs only on devin; {engine} takes a normal model, e.g. {}",
            default_for(engine)
                .map(|m| m.json())
                .unwrap_or_else(|| "none".into())
        )),
        _ => Err(format!("unknown engine {engine}")),
    }
}
fn whole(id: String) -> ResolvedModel {
    ResolvedModel {
        model: id.clone(),
        id,
        effort: None,
    }
}

/// Composes `choice` and checks the id against `catalog`, the ids the engine lists. An
/// unknown id names the nearest listed ones.
pub fn resolve(
    engine: &str,
    choice: &ModelChoice,
    catalog: &[String],
) -> Result<ResolvedModel, String> {
    let resolved = compose(engine, choice)?;
    if catalog.contains(&resolved.id) {
        return Ok(resolved);
    }
    let near = nearest(&resolved.id, catalog);
    Err(format!(
        "{engine} has no model {} (composed from model {}); {}",
        resolved.id,
        choice.json(),
        if near.is_empty() {
            format!("{engine} listed no models")
        } else {
            format!("nearest: {}", near.join(", "))
        }
    ))
}

/// Up to [`NEAREST`] catalog ids closest to `id`: the ids it begins first (`swe-2` →
/// `swe-2-high`), then fewest edits, then the longest shared prefix, then by name.
pub fn nearest(id: &str, catalog: &[String]) -> Vec<String> {
    let mut ranked: Vec<_> = catalog
        .iter()
        .map(|c| {
            let prefix = id
                .chars()
                .zip(c.chars())
                .take_while(|(a, b)| a == b)
                .count();
            (
                !c.starts_with(id),
                distance(id, c),
                std::cmp::Reverse(prefix),
                c,
            )
        })
        .collect();
    ranked.sort();
    ranked.dedup_by(|a, b| a.3 == b.3);
    ranked
        .into_iter()
        .take(NEAREST)
        .map(|(_, _, _, c)| c.clone())
        .collect()
}
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(ca != *cb));
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// The `model` a request names, from its `model` and any leftover `effort` input. Absent (or
/// null) is the engine default. A string or other non-object `model`, or an `effort` input,
/// is the retired form: refused with the object to use instead. The object's engine fit
/// (fusion only on Devin, no `fast` on Codex or Claude) is checked here too.
pub fn from_inputs(
    engine: &str,
    model: Option<&Value>,
    effort: Option<&Value>,
) -> Result<Option<ModelChoice>, String> {
    let model = model.filter(|v| !v.is_null());
    let effort = effort.filter(|v| !v.is_null());
    let stray = effort.map(|v| {
        v.as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| v.to_string())
    });
    match model {
        Some(value) if value.is_object() => {
            let choice = ModelChoice::parse(value).map_err(|e| {
                format!(
                    "{e}; for example {}",
                    default_for(engine)
                        .map(|m| m.json())
                        .unwrap_or_else(|| "none".into())
                )
            })?;
            if let Some(effort) = stray {
                return Err(format!(
                    "effort is no longer an input: drop it and set model to {}",
                    with_effort(choice, &effort).json()
                ));
            }
            compose(engine, &choice)?;
            Ok(Some(choice))
        }
        Some(value) => {
            let hint = suggest(engine, value, stray.as_deref());
            Err(match stray {
                None => format!(
                    "model must be a JSON object, not {value}; use {}",
                    hint.json()
                ),
                Some(_) => format!(
                    "model must be a JSON object, not {value}, and effort is no longer an input: drop effort and set model to {}",
                    hint.json()
                ),
            })
        }
        None => match stray {
            None => Ok(None),
            Some(effort) => Err(format!(
                "effort is no longer an input: drop it and set model to {}",
                default_for(engine)
                    .map(|m| with_effort(m, &effort).json())
                    .unwrap_or_else(|| "none".into())
            )),
        },
    }
}
fn with_effort(choice: ModelChoice, effort: &str) -> ModelChoice {
    match choice {
        ModelChoice::Normal(main) => ModelChoice::Normal(Main {
            effort: Some(effort.into()),
            ..main
        }),
        fusion => fusion,
    }
}
/// The fusion object the retired `"fusion"` string stands for.
fn fusion_default() -> ModelChoice {
    ModelChoice::Fusion {
        main: Main {
            model: "claude-opus-5-5".into(),
            effort: Some("high".into()),
            fast: false,
        },
        sidekick: Sidekick {
            model: "swe-2".into(),
            effort: Some("high".into()),
            priority: false,
        },
    }
}
/// A retired Devin id taken apart: `M[-E][-fast]` as a main, `S[-F][-priority]` as a sidekick.
fn split(id: &str, flag: &str) -> (String, Option<String>, bool) {
    let (rest, flagged) = match id.strip_suffix(&format!("-{flag}")) {
        Some(rest) => (rest, true),
        None => (id, false),
    };
    match rest.rsplit_once('-') {
        Some((model, effort)) if !model.is_empty() && EFFORT_WORDS.contains(&effort) => {
            (model.into(), Some(effort.into()), flagged)
        }
        _ => (rest.into(), None, flagged),
    }
}
/// The object a retired value meant: `"sol"` → sol at the given or high effort, `"fusion"`
/// → [`fusion_default`], a Devin id such as `swe-2-high` or a fusion id taken apart.
fn suggest(engine: &str, value: &Value, effort: Option<&str>) -> ModelChoice {
    let fallback = || {
        let default = default_for(engine).unwrap_or_else(|| ModelChoice::normal("swe-2", None));
        match effort {
            Some(effort) => with_effort(default, effort),
            None => default,
        }
    };
    let Some(name) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return fallback();
    };
    if EFFORT_WORDS.contains(&name) {
        return default_for(engine)
            .map(|m| with_effort(m, name))
            .unwrap_or_else(fallback);
    }
    if matches!(engine, "codex" | "claude") {
        return ModelChoice::normal(name, Some(effort.unwrap_or("high")));
    }
    if name == "fusion" {
        return fusion_default();
    }
    if let Some((main, sidekick)) = name
        .strip_prefix("fusion-")
        .and_then(|rest| rest.split_once("-sidekick-"))
    {
        let (model, main_effort, fast) = split(main, "fast");
        let (side, side_effort, priority) = split(sidekick, "priority");
        return ModelChoice::Fusion {
            main: Main {
                model,
                effort: main_effort,
                fast,
            },
            sidekick: Sidekick {
                model: side,
                effort: side_effort,
                priority,
            },
        };
    }
    let (model, own, fast) = split(name, "fast");
    ModelChoice::Normal(Main {
        model,
        effort: own.or_else(|| effort.map(str::to_owned)),
        fast,
    })
}

/// Per engine and executable: when the listing was fetched, and its ids.
type Catalogs = HashMap<(String, PathBuf), (Instant, Arc<[String]>)>;
static CATALOGS: LazyLock<Mutex<Catalogs>> = LazyLock::new(Default::default);

/// The engine's listing from `fetch`, reused for [`CATALOG_TTL`] per engine and executable
/// within this process.
pub async fn cached(
    engine: &str,
    binary: &Path,
    fetch: impl Future<Output = Result<Vec<String>, EngineError>>,
) -> Result<Vec<String>, EngineError> {
    let key = (engine.to_owned(), binary.to_owned());
    if let Some((at, list)) = CATALOGS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        && at.elapsed() < CATALOG_TTL
    {
        return Ok(list.to_vec());
    }
    let list: Arc<[String]> = fetch.await?.into();
    CATALOGS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, (Instant::now(), list.clone()));
    Ok(list.to_vec())
}

/// Runs an engine's model-listing command (bounded to a minute and 8 MiB) and returns its
/// stdout. Any failure is fatal to the launch: an unlisted model never runs.
pub(crate) async fn listing(
    mut command: tokio::process::Command,
    what: &str,
) -> Result<Vec<u8>, EngineError> {
    let fail = |message: String| EngineError {
        kind: EngineErrorKind::Fatal,
        message: format!("could not list {what}: {message}"),
        retry_at: None,
    };
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = tokio::time::timeout(Duration::from_secs(60), command.output())
        .await
        .map_err(|_| fail("timed out after 60 s".into()))?
        .map_err(|e| fail(e.to_string()))?;
    if !out.status.success() {
        return Err(fail(format!(
            "{}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    if out.stdout.len() > 8 << 20 {
        return Err(fail("listing exceeds 8 MiB".into()));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn distance_counts_edits() {
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("same", "same"), 0);
    }
    #[test]
    fn parse_refuses_unknown_fields_and_types() {
        for (value, message) in [
            (json!({"type":"normal"}), "model.model is required"),
            (json!({"type":"other","model":"x"}), "model.type"),
            (
                json!({"type":"normal","model":"x","priority":true}),
                "no field \"priority\"",
            ),
            (
                json!({"type":"normal","model":"x","effort":null}),
                "model.effort must be a non-empty string",
            ),
            (
                json!({"type":"normal","model":"x","fast":"yes"}),
                "model.fast must be true or false",
            ),
            (
                json!({"type":"fusion","main":{"model":"m"},"sidekick":{"model":"s","fast":true}}),
                "model.sidekick has no field \"fast\"",
            ),
            (
                json!({"type":"fusion","main":{"model":"m"},"sidekick":{"model":"s","priority":1}}),
                "model.sidekick.priority must be true or false",
            ),
            (
                json!({"type":"fusion","main":{"model":"m","effort":"high","fast":"yes"},"sidekick":{"model":"s"}}),
                "model.main.fast must be true or false",
            ),
            (
                json!({"type":"fusion","main":{"effort":"high"},"sidekick":{"model":"s"}}),
                "model.main.model is required",
            ),
            (
                json!({"type":"normal","model":""}),
                "model.model must be a non-empty string",
            ),
        ] {
            let error = ModelChoice::parse(&value).unwrap_err();
            assert!(error.contains(message), "{value}: {error}");
        }
    }
    #[test]
    fn serialization_round_trips_and_omits_defaults() {
        let value = json!({"type":"fusion","main":{"model":"claude-opus-5-5","effort":"high"},"sidekick":{"model":"swe-2","effort":"high"}});
        let choice = ModelChoice::parse(&value).unwrap();
        assert_eq!(choice.to_value(), value);
        assert_eq!(
            serde_json::from_value::<ModelChoice>(value).unwrap(),
            choice
        );
    }
}
