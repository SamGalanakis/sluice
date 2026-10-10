//! Read-only verification. Registry inspection, JSON/type replay and file checks never dispatch fns.
use crate::calls::{parse_id, public, validate_fields};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sluice_model::{
    commands::StepStatus,
    error::PublicError,
    gates::{StateSnapshot, StepState},
    hash::InputsHash,
    ids::{ProjectId, ProjectSelector, StepId},
    plan::{Binding, FnSignature, Pause, Plan, compile_rows, output_references, step_index},
    plan_rows::{PlanRows, ReferenceRow},
    rpc::{JsonMap, decode_json},
    types::{Type, check_value_at},
};
use sluice_store::{ReadPool, messages};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    pub r#where: String,
    pub message: String,
}
#[derive(Debug, Clone, Default)]
pub struct RegistryInspection {
    pub signatures: IndexMap<String, FnSignature>,
    pub problems: Vec<Problem>,
}
/// The registry owner supplies all visible signatures and independent broken-entry diagnostics.
pub trait VerificationRegistry: Send + Sync + 'static {
    fn inspect(&self, project: Option<ProjectId>) -> RegistryInspection;
}
#[derive(Clone, Default)]
pub struct FixedRegistry(pub IndexMap<String, FnSignature>);
impl VerificationRegistry for FixedRegistry {
    fn inspect(&self, _project: Option<ProjectId>) -> RegistryInspection {
        RegistryInspection {
            signatures: self.0.clone(),
            problems: vec![],
        }
    }
}

fn add(problems: &mut Vec<Problem>, at: impl Into<String>, message: impl Into<String>) {
    let p = Problem {
        r#where: at.into(),
        message: message.into(),
    };
    if !problems.contains(&p) {
        problems.push(p);
    }
}
fn errors(problems: &mut Vec<Problem>, at: &str, e: PublicError) {
    if let PublicError::Invalid { errors, message } = e {
        if errors.is_empty() {
            add(problems, at, message);
        }
        for e in errors {
            let (path, msg) = e.split_once(": ").unwrap_or(("", &e));
            add(
                problems,
                if path.is_empty() {
                    at.into()
                } else {
                    format!("{at}#{path}")
                },
                msg,
            );
        }
    } else {
        add(problems, at, e.to_string());
    }
}
fn parse<T: serde::de::DeserializeOwned>(
    text: &str,
    at: &str,
    problems: &mut Vec<Problem>,
) -> Option<T> {
    match decode_json(text.as_bytes()) {
        Ok(v) => Some(v),
        Err(e) => {
            add(problems, at, e.to_string());
            None
        }
    }
}
#[derive(Debug)]
struct ProjectData {
    id: ProjectId,
    name: String,
    paused: bool,
    /// The plan's authored rows, `None` when the project has no plan row.
    rows: Option<PlanRows>,
    /// The rebuildable indexes as stored (plan-rows §2.3).
    stored: Indexes,
    inputs: Vec<String>,
    steps: Vec<String>,
    attempts: Vec<String>,
    results: Vec<String>,
    resources: Vec<String>,
}

/// Every relational read belongs to one read-only snapshot. Files and registry bundles
/// are inspected on a blocking task after that snapshot; neither is execution truth.
pub async fn verify<R: VerificationRegistry>(
    reads: &ReadPool,
    registry: Arc<R>,
    project: Option<ProjectSelector>,
) -> Result<Vec<Problem>, PublicError> {
    let selected = project;
    let data=reads.snapshot(move|sql|{
        let selected=selected.map(|s|messages::resolve_project(sql,&s)).transpose()?;
        let mut stmt=sql.prepare("SELECT project_id,name,paused FROM projects WHERE deleted_at IS NULL AND (?1 IS NULL OR project_id=?1) ORDER BY name")?;
        let rows=stmt.query_map([selected.map(|p|p.to_string())],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,bool>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
        let mut data=vec![];
        for (id,name,paused) in rows{
            let id:ProjectId=parse_id(id)?;
            let planned:bool=sql.query_row("SELECT EXISTS(SELECT 1 FROM plans WHERE project_id=?1)",[id.to_string()],|r|r.get(0))?;
            let rows=if planned {Some(sluice_store::plans::read_plan_rows(sql,id)?)} else {None};
            let stored=Indexes::read(sql,id)?;
            let mut inputs=vec![];let mut steps=vec![];let mut attempts=vec![];let mut results=vec![];let mut resources=vec![];
            for (query,target) in [
                ("SELECT json_object('name',name,'declaration',json(declaration),'value',json(value),'has_value',value IS NOT NULL) FROM inputs WHERE project_id=?1 ORDER BY position",&mut inputs),
                ("SELECT json_object('id',step_id,'declaration',json(declaration),'status',status,'outputs',json(outputs),'inputs_hash',inputs_hash,'manual',manual,'result_id',result_id,'generation',generation,'work',work_generation) FROM steps WHERE project_id=?1 ORDER BY position",&mut steps),
                ("SELECT json_object('id',attempt_id,'step',step_id,'request',json(request),'inputs_hash',inputs_hash,'phase',phase,'run',(SELECT run_id FROM runs WHERE attempt_id=a.attempt_id)) FROM attempts a WHERE project_id=?1 ORDER BY created_at",&mut attempts),
                ("SELECT json_object('id',result_id,'step',step_id,'declaration',json(declaration),'inputs',json(inputs),'inputs_hash',inputs_hash,'status',status,'outputs',json(outputs),'attempt',attempt_id,'manual',manual) FROM step_results WHERE project_id=?1 ORDER BY recorded_at",&mut results),
                ("SELECT json_object('name',name,'declaration',json(declaration)) FROM resources WHERE project_id=?1 ORDER BY name",&mut resources),
            ]{
                let mut stmt=sql.prepare(query)?;
                *target=stmt.query_map([id.to_string()],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            }
            data.push(ProjectData{id,name,paused,rows,stored,inputs,steps,attempts,results,resources});
        }
        // Home calls have frozen attempts as well, without a project plan.
        let mut stmt=sql.prepare("SELECT json_object('id',attempt_id,'step',step_id,'request',json(request),'inputs_hash',inputs_hash,'phase',phase,'run',(SELECT run_id FROM runs WHERE attempt_id=a.attempt_id)) FROM attempts a WHERE project_id IS NULL ORDER BY created_at")?;
        let home_attempts=stmt.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        Ok((data,home_attempts,selected.is_none()))
    }).await.map_err(public)?;
    let home = reads.home().to_owned();
    tokio::task::spawn_blocking(move || {
        let (data, home_attempts, whole_home) = data;
        let mut problems = registry.inspect(None).problems;
        check_env(&home.join(".env"), ".env", &mut problems);
        if whole_home {
            let ids: Vec<_> = data.iter().map(|d| d.id.to_string()).collect();
            match std::fs::read_dir(home.join("projects")) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(e) => {
                                if e.path().is_dir()
                                    && !ids.contains(&e.file_name().to_string_lossy().into_owned())
                                {
                                    add(
                                        &mut problems,
                                        format!("projects/{}", e.file_name().to_string_lossy()),
                                        "warning: directory belongs to no live project",
                                    );
                                }
                            }
                            Err(e) => add(&mut problems, "projects", e.to_string()),
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => add(&mut problems, "projects", e.to_string()),
            }
        }
        for raw in home_attempts {
            if let Some(a) = parse::<Value>(&raw, "home: attempts", &mut problems) {
                check_attempt(&a, "home", &mut problems);
            }
        }
        for d in data {
            check_project(d, &home, registry.as_ref(), &mut problems);
        }
        problems
    })
    .await
    .map_err(|e| PublicError::Storage {
        message: e.to_string(),
    })
}
fn check_project(
    d: ProjectData,
    home: &Path,
    registry: &impl VerificationRegistry,
    problems: &mut Vec<Problem>,
) {
    let at = format!("project {} ({})", d.name, d.id);
    let reg = registry.inspect(Some(d.id));
    for p in reg.problems {
        add(problems, p.r#where, p.message);
    }
    check_env(
        &home.join("projects").join(d.id.to_string()).join(".env"),
        &format!("projects/{}/.env", d.id),
        problems,
    );
    let plan = match &d.rows {
        None => {
            add(problems, format!("{at}: plan"), "plan is missing");
            None
        }
        Some(rows) => match compile_rows(rows, &reg.signatures) {
            Ok(p) => Some(p),
            Err(e) => {
                for e in e {
                    add(problems, format!("{at}: plan#{}", e.path), e.message);
                }
                None
            }
        },
    };
    if let Some(rows) = &d.rows {
        let rebuilt = Indexes::rebuild(rows, plan.as_ref());
        compare_indexes(&at, &rebuilt, &d.stored, plan.is_some(), problems);
    }
    let mut state = StateSnapshot {
        paused: if d.paused { Pause::Yes } else { Pause::No },
        ..StateSnapshot::default()
    };
    for raw in d.inputs {
        let Some(v) = parse::<Value>(&raw, &format!("{at}: inputs"), problems) else {
            continue;
        };
        let name = v["name"].as_str().unwrap_or("?");
        if v["has_value"] == 1 {
            if let Ok(value) = v["value"].clone().try_into() {
                state.inputs.0.insert(name.into(), value);
            }
            if let Some(plan) = &plan {
                match plan.inputs().get(name) {
                    Some(decl) => {
                        if let Err(e) =
                            check_value_at(&decl.ty, &v["value"], &format!("inputs.{name}"))
                        {
                            for e in e {
                                add(problems, format!("{at}: state#{}", e.path), e.message);
                            }
                        }
                    }
                    None => add(
                        problems,
                        format!("{at}: state#inputs.{name}"),
                        "value is not declared by the plan",
                    ),
                }
            }
        }
    }
    let mut projections = vec![];
    for raw in d.steps {
        let Some(v) = parse::<Value>(&raw, &format!("{at}: steps"), problems) else {
            continue;
        };
        let id = v["id"].as_str().unwrap_or("?");
        let step_at = format!("{at}: state#steps.{id}");
        let status = serde_json::from_value::<StepStatus>(v["status"].clone());
        let outputs = serde_json::from_value::<Option<JsonMap>>(v["outputs"].clone());
        match (id.parse::<StepId>(), status, outputs) {
            (Ok(id), Ok(status), Ok(outputs)) => {
                state.steps.insert(
                    id.clone(),
                    StepState {
                        status: status.clone(),
                        outputs: outputs.clone().unwrap_or_default(),
                        ..StepState::default()
                    },
                );
                if let Some(plan) = &plan {
                    if let Some(step) = plan.steps().get(&id) {
                        if matches!(status, StepStatus::Succeeded | StepStatus::Stale) {
                            let schema = step
                                .signature
                                .outputs
                                .iter()
                                .chain(step.declared_outputs.iter().map(|(k, d)| (k, &d.ty)))
                                .map(|(k, _)| {
                                    (k.clone(), step.output_type(k).expect("compiled output"))
                                })
                                .collect();
                            if let Err(e) =
                                validate_fields(&schema, &outputs.unwrap_or_default(), "outputs")
                            {
                                errors(problems, &step_at, e);
                            }
                        }
                    } else {
                        add(problems, &step_at, "state for a step absent from the plan");
                    }
                }
            }
            _ => add(
                problems,
                &step_at,
                "invalid step identity, status or outputs",
            ),
        }
        projections.push(v);
    }
    if let Some(plan) = &plan {
        for (id, step) in plan.steps() {
            if !state.steps.contains_key(id) {
                add(
                    problems,
                    format!("{at}: state#steps.{id}"),
                    "step projection is missing",
                );
            }
            for (name, binding) in &step.bindings {
                if let Binding::File(path) = binding {
                    match std::fs::metadata(path).and_then(|m| {
                        if !m.is_file() {
                            return Err(std::io::Error::other("not a regular file"));
                        }
                        std::fs::File::open(path)
                    }) {
                        Ok(file) => {
                            if !file.metadata().is_ok_and(|m| m.is_file()) {
                                add(
                                    problems,
                                    format!("{at}: plan#steps.{id}.in.{name}.file"),
                                    "warning: file binding is not a regular readable file",
                                );
                            }
                        }
                        Err(e) => add(
                            problems,
                            format!("{at}: plan#steps.{id}.in.{name}.file"),
                            format!("warning: file is not readable: {e}"),
                        ),
                    }
                }
            }
        }
        for v in &projections {
            if v["status"] == "succeeded"
                && v["manual"] == 0
                && let Some(id) = v["id"].as_str().and_then(|s| s.parse::<StepId>().ok())
                && let Some(step) = plan.steps().get(&id)
            {
                let expected =
                    sluice_model::plan::inputs_hash(plan, &state, step).map(|h| h.to_string());
                if expected.as_deref() != v["inputs_hash"].as_str() {
                    add(
                        problems,
                        format!("{at}: state#steps.{id}.inputs_hash"),
                        "succeeded step hash differs from effective data; result is stale",
                    );
                }
            }
        }
    }
    let mut frozen = IndexMap::<String, Value>::new();
    for raw in d.attempts {
        if let Some(v) = parse::<Value>(&raw, &format!("{at}: attempts"), problems) {
            check_attempt(&v, &at, problems);
            if let Some(id) = v["id"].as_str() {
                frozen.insert(id.into(), v["request"].clone());
            }
        }
    }
    for raw in d.results {
        let Some(v) = parse::<Value>(&raw, &format!("{at}: results"), problems) else {
            continue;
        };
        let result_at = format!("{at}: results#{}", v["id"].as_str().unwrap_or("?"));
        if let Some(inputs) = v["inputs"].as_object() {
            let inputs: JsonMap = serde_json::from_value(Value::Object(inputs.clone()))
                .expect("strict snapshot values");
            if InputsHash::of(&inputs)
                .ok()
                .map(|h| h.to_string())
                .as_deref()
                != v["inputs_hash"].as_str()
            {
                add(
                    problems,
                    format!("{result_at}.inputs_hash"),
                    "result hash differs from its frozen effective inputs",
                );
            }
        }
        if v["status"] == "succeeded" || v["status"] == "stale" {
            let schema = if let Some(a) = v["attempt"].as_str().and_then(|a| frozen.get(a)) {
                merged_schema(a)
            } else {
                let mut schema = reg
                    .signatures
                    .get(v["declaration"]["run"].as_str().unwrap_or(""))
                    .map(|s| s.outputs.clone())
                    .unwrap_or_default();
                if let Some(o) = v["declaration"]["outputs"].as_object() {
                    for (k, v) in o {
                        if let Ok(ty) = Type::parse(v.get("type").unwrap_or(v)) {
                            schema.insert(k.clone(), ty);
                        }
                    }
                }
                Ok(schema)
            };
            match (
                schema,
                serde_json::from_value::<JsonMap>(v["outputs"].clone()),
            ) {
                (Ok(schema), Ok(outputs)) => {
                    let schema = if v["declaration"]["scatter"].is_string() {
                        schema
                            .into_iter()
                            .map(|(k, t)| (k, Type::List(Box::new(t))))
                            .collect()
                    } else {
                        schema
                    };
                    if let Err(e) = validate_fields(&schema, &outputs, "outputs") {
                        errors(problems, &result_at, e);
                    }
                }
                _ => add(problems, &result_at, "invalid result output/schema"),
            }
        }
    }
    for raw in d.resources {
        if let Some(v) = parse::<Value>(&raw, &format!("{at}: resources"), problems)
            && let Some(name) = v["declaration"]["capacity_fn"].as_str()
        {
            match reg.signatures.get(name) {
                Some(s)
                    if matches!(s.outputs.get("capacity"), Some(Type::Int | Type::Any))
                        && s.inputs.values().all(|t| matches!(t, Type::Optional(_))) => {}
                _ => add(
                    problems,
                    format!("{at}: resources#{}", v["name"].as_str().unwrap_or("?")),
                    "capacity fn must have no required inputs and an int capacity output",
                ),
            }
        }
    }
}
/// A plan's rebuildable indexes (plan-rows §2.3) as comparable sets: `steps.unit` by step,
/// `step_tags` rows, `plan_refs` rows and `plan_edges` rows, each in its stored spelling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Indexes {
    /// step → unit.
    pub units: std::collections::BTreeMap<String, String>,
    /// (step, tag).
    pub tags: std::collections::BTreeSet<(String, String)>,
    /// (consumer kind, consumer, slot, ordinal, kind, source kind, source, port, path).
    pub refs: std::collections::BTreeSet<RefRow>,
    /// (source step, target step, kind, via unit or "").
    pub edges: std::collections::BTreeSet<(String, String, String, String)>,
}
/// A `plan_refs` row in its stored spelling.
pub type RefRow = (
    String,
    String,
    String,
    u32,
    String,
    String,
    String,
    String,
    String,
);
fn word<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
fn ref_row(row: &ReferenceRow) -> RefRow {
    (
        word(&row.consumer_kind),
        row.consumer_id.clone(),
        row.slot.clone(),
        row.ordinal,
        word(&row.kind),
        word(&row.source_kind),
        row.source_id.clone(),
        row.source_port.clone(),
        row.source_path.clone(),
    )
}
impl Indexes {
    /// The indexes as stored, in the caller's snapshot.
    fn read(sql: &rusqlite::Connection, project: ProjectId) -> sluice_store::Result<Self> {
        let p = project.to_string();
        let mut out = Self::default();
        let mut stmt = sql.prepare("SELECT step_id,unit FROM steps WHERE project_id=?1")?;
        for row in stmt.query_map([&p], |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (step, unit) = row?;
            out.units.insert(step, unit);
        }
        let mut stmt = sql.prepare("SELECT step_id,tag FROM step_tags WHERE project_id=?1")?;
        for row in stmt.query_map([&p], |r| Ok((r.get(0)?, r.get(1)?)))? {
            out.tags.insert(row?);
        }
        let mut stmt = sql.prepare(
            "SELECT consumer_kind,consumer_id,slot,ordinal,kind,source_kind,source_id,source_port,source_path
             FROM plan_refs WHERE project_id=?1",
        )?;
        for row in stmt.query_map([&p], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })? {
            out.refs.insert(row?);
        }
        let mut stmt = sql.prepare(
            "SELECT source_step,target_step,kind,via_unit FROM plan_edges WHERE project_id=?1",
        )?;
        for row in stmt.query_map([&p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))? {
            out.edges.insert(row?);
        }
        Ok(out)
    }
    /// The indexes rebuilt from the declarations alone (plan-rows §2.5): each step's
    /// `step_index`, each plan output's `output_references`, and the edges those references
    /// expand to. A unit gate expands through the unit's current exit steps, which only a
    /// compiled plan knows: with no plan (it does not compile) the edges are not rebuilt.
    pub fn rebuild(rows: &PlanRows, plan: Option<&Plan>) -> Self {
        let steps: std::collections::HashSet<&str> =
            rows.steps.iter().map(|s| s.step.as_str()).collect();
        let is_step = |name: &str| steps.contains(name);
        let mut out = Self::default();
        for row in &rows.steps {
            let index = step_index(&row.step, &row.declaration, &is_step);
            out.units
                .insert(row.step.to_string(), index.unit.to_string());
            for tag in index.tags {
                out.tags.insert((row.step.to_string(), tag));
            }
            out.refs.extend(index.references.iter().map(ref_row));
        }
        for row in &rows.outputs {
            out.refs.extend(
                output_references(&row.name, &row.binding)
                    .iter()
                    .map(ref_row),
            );
        }
        if let Some(plan) = plan {
            out.edges = derive_edges(&out.refs, &steps, plan);
        }
        out
    }
}
/// `plan_edges` from the step consumers' references (plan-rows §2.5): a `data` edge per
/// distinct step a binding reads, a `gate` edge per distinct step a gate names or reads a
/// boolean from, and for each `unit:u` gate a `gate` edge from each current exit step of `u`
/// with `via_unit` u. Both ends are current steps.
fn derive_edges(
    refs: &std::collections::BTreeSet<RefRow>,
    steps: &std::collections::HashSet<&str>,
    plan: &Plan,
) -> std::collections::BTreeSet<(String, String, String, String)> {
    let mut edges = std::collections::BTreeSet::new();
    for (consumer_kind, target, _, _, kind, source_kind, source, _, _) in refs {
        if consumer_kind != "step" || !steps.contains(target.as_str()) {
            continue;
        }
        match (kind.as_str(), source_kind.as_str()) {
            ("binding", "step") if steps.contains(source.as_str()) => {
                edges.insert((source.clone(), target.clone(), "data".into(), String::new()));
            }
            ("gate", "step") if steps.contains(source.as_str()) => {
                edges.insert((source.clone(), target.clone(), "gate".into(), String::new()));
            }
            ("gate", "unit") => {
                let exits = source
                    .parse::<sluice_model::ids::UnitName>()
                    .ok()
                    .and_then(|unit| plan.units().get(&unit))
                    .map(|unit| unit.exits.clone())
                    .unwrap_or_default();
                for exit in exits {
                    edges.insert((
                        exit.to_string(),
                        target.clone(),
                        "gate".into(),
                        source.clone(),
                    ));
                }
            }
            _ => {}
        }
    }
    edges
}
/// Each difference between the rebuilt and the stored indexes, as a problem at the index row.
/// `edges` is false when the plan does not compile: its edges could not be rebuilt
/// (`Indexes::rebuild`), so the stored ones are not compared.
pub fn compare_indexes(
    at: &str,
    rebuilt: &Indexes,
    stored: &Indexes,
    edges: bool,
    problems: &mut Vec<Problem>,
) {
    let mut steps: std::collections::BTreeSet<&String> = rebuilt.units.keys().collect();
    steps.extend(stored.units.keys());
    for step in steps {
        let (want, have) = (rebuilt.units.get(step), stored.units.get(step));
        if want != have {
            add(
                problems,
                format!("{at}: index#steps.{step}.unit"),
                format!(
                    "stored unit {} differs from the declaration's {}",
                    have.map_or("(none)", String::as_str),
                    want.map_or("(none)", String::as_str)
                ),
            );
        }
    }
    for tag in rebuilt.tags.symmetric_difference(&stored.tags) {
        let what = if stored.tags.contains(tag) {
            "stored tag the declaration lacks"
        } else {
            "declared tag missing from the index"
        };
        add(
            problems,
            format!("{at}: index#step_tags.{}.{}", tag.0, tag.1),
            what,
        );
    }
    for row in rebuilt.refs.symmetric_difference(&stored.refs) {
        let what = if stored.refs.contains(row) {
            "stored reference the declarations do not make"
        } else {
            "declared reference missing from the index"
        };
        add(
            problems,
            format!(
                "{at}: index#plan_refs.{}.{}.{}[{}]",
                row.0, row.1, row.2, row.3
            ),
            format!(
                "{what}: {} {} {}{}{}",
                row.4,
                row.5,
                row.6,
                if row.7.is_empty() {
                    String::new()
                } else {
                    format!("/{}", row.7)
                },
                if row.8.is_empty() {
                    String::new()
                } else {
                    format!(".{}", row.8)
                }
            ),
        );
    }
    if !edges {
        return;
    }
    for edge in rebuilt.edges.symmetric_difference(&stored.edges) {
        let what = if stored.edges.contains(edge) {
            "stored edge the declarations do not make"
        } else {
            "declared edge missing from the index"
        };
        let via = if edge.3.is_empty() {
            String::new()
        } else {
            format!(" via unit:{}", edge.3)
        };
        add(
            problems,
            format!("{at}: index#plan_edges.{}", edge.1),
            format!("{what}: {} {} after {}{via}", edge.2, edge.1, edge.0),
        );
    }
}
fn merged_schema(request: &Value) -> Result<IndexMap<String, Type>, PublicError> {
    let mut schema = IndexMap::new();
    for key in ["returns", "declared"] {
        let values = request[key]
            .as_object()
            .ok_or_else(|| PublicError::BadRequest {
                message: format!("frozen {key} schema missing"),
            })?;
        for (name, ty) in values {
            schema.insert(
                name.clone(),
                Type::parse(ty).map_err(|e| PublicError::BadRequest {
                    message: e.to_string(),
                })?,
            );
        }
    }
    Ok(schema)
}
fn check_attempt(v: &Value, at: &str, problems: &mut Vec<Problem>) {
    let at = format!("{at}: attempts#{}", v["id"].as_str().unwrap_or("?"));
    let request = &v["request"];
    if v["run"].is_null() {
        add(problems, &at, "attempt has no run identity");
    }
    let inputs = request
        .get("effective_inputs")
        .unwrap_or(&request["inputs"]);
    match serde_json::from_value::<JsonMap>(inputs.clone()) {
        Ok(inputs) => {
            if InputsHash::of(&inputs)
                .ok()
                .map(|h| h.to_string())
                .as_deref()
                != v["inputs_hash"].as_str()
            {
                add(
                    problems,
                    format!("{at}.inputs_hash"),
                    "attempt hash differs from frozen effective inputs",
                );
            }
        }
        Err(e) => add(problems, format!("{at}.inputs"), e.to_string()),
    }
    if let Some(function) = request.get("function") {
        match serde_json::from_value::<crate::calls::FrozenFunction>(function.clone()) {
            Ok(f) => match serde_json::from_value::<JsonMap>(request["inputs"].clone()) {
                Ok(i) => {
                    if let Err(e) = validate_fields(&f.inputs, &i, "inputs") {
                        errors(problems, &at, e);
                    }
                }
                Err(e) => add(problems, &at, e.to_string()),
            },
            Err(e) => add(problems, &at, e.to_string()),
        }
    } else {
        if let Err(e) = merged_schema(request) {
            errors(problems, &at, e);
        }
        // Validate frozen execution inputs using their own manifest, independent of later edits.
        if !request["inputs"].is_object() {
            add(
                problems,
                format!("{at}.inputs"),
                "frozen execution inputs are not an object",
            );
        }
    }
}
fn check_env(path: &Path, at: &str, problems: &mut Vec<Problem>) {
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            add(problems, at, e.to_string());
            return;
        }
    };
    for line in crate::dotenv::parse(&text).1 {
        add(problems, format!("{at}:{line}"), "not a KEY=value line");
    }
}

/// Filesystem adapter for the registry owner: preserve precedence and diagnose every entry.
/// Supplied roots are global, configured directories, then the immutable project directory.
pub fn inspect_fns(
    home: &Path,
    roots: &[PathBuf],
    builtins: IndexMap<String, FnSignature>,
) -> RegistryInspection {
    let mut inspection = RegistryInspection {
        signatures: builtins,
        problems: vec![],
    };
    for root in roots {
        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                add(
                    &mut inspection.problems,
                    root.display().to_string(),
                    e.to_string(),
                );
                continue;
            }
        };
        let mut dirs = vec![];
        for e in entries {
            match e {
                Ok(e) if e.path().is_dir() => dirs.push(e.path()),
                Ok(_) => {}
                Err(e) => add(
                    &mut inspection.problems,
                    root.display().to_string(),
                    e.to_string(),
                ),
            }
        }
        dirs.sort();
        for dir in dirs {
            let previous_problems = inspection.problems.len();
            let path = dir.join("fn.json");
            let at = path
                .strip_prefix(home)
                .unwrap_or(&path)
                .display()
                .to_string();
            let raw = match std::fs::read(&path) {
                Ok(raw) => raw,
                Err(e) => {
                    add(&mut inspection.problems, &at, e.to_string());
                    continue;
                }
            };
            let value = match decode_json::<Value>(&raw) {
                Ok(v) => v,
                Err(e) => {
                    add(&mut inspection.problems, &at, e.to_string());
                    continue;
                }
            };
            let Some(obj) = value.as_object() else {
                add(
                    &mut inspection.problems,
                    &at,
                    "expected a fn manifest object",
                );
                continue;
            };
            for key in obj.keys() {
                if ![
                    "name", "doc", "inputs", "outputs", "open", "submits", "icon",
                ]
                .contains(&key.as_str())
                {
                    add(
                        &mut inspection.problems,
                        &at,
                        format!("unknown key '{key}'"),
                    );
                }
            }
            let Some(name) = value["name"].as_str() else {
                add(&mut inspection.problems, &at, "fn name is missing");
                continue;
            };
            if Some(name) != dir.file_name().and_then(|s| s.to_str()) {
                add(
                    &mut inspection.problems,
                    &at,
                    "fn name does not match its directory",
                );
            }
            if inspection.signatures.contains_key(name) {
                add(
                    &mut inspection.problems,
                    &at,
                    format!("fn {name} collides with an earlier scope"),
                );
                continue;
            }
            if !dir.join("main.py").is_file() {
                add(&mut inspection.problems, &at, "main.py is missing");
            }
            let mut signature = FnSignature::default();
            for (field, target) in [
                ("inputs", &mut signature.inputs),
                ("outputs", &mut signature.outputs),
            ] {
                if let Some(values) = value[field].as_object() {
                    for (name, v) in values {
                        match Type::parse(v.get("type").unwrap_or(v)) {
                            Ok(t) => {
                                target.insert(name.clone(), t);
                            }
                            Err(e) => add(
                                &mut inspection.problems,
                                &at,
                                format!("{field}.{name}: {e}"),
                            ),
                        }
                    }
                } else {
                    add(
                        &mut inspection.problems,
                        &at,
                        format!("{field}: expected an object"),
                    );
                }
            }
            signature.open = value["open"].as_bool().unwrap_or(false);
            if inspection.problems.len() == previous_problems {
                inspection.signatures.insert(name.into(), signature);
            }
        }
    }
    inspection
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(
        consumer: &str,
        slot: &str,
        ordinal: u32,
        kind: &str,
        source: (&str, &str, &str, &str),
    ) -> RefRow {
        let consumer_kind = if kind == "output" { "output" } else { "step" };
        (
            consumer_kind.into(),
            consumer.into(),
            slot.into(),
            ordinal,
            kind.into(),
            source.0.into(),
            source.1.into(),
            source.2.into(),
            source.3.into(),
        )
    }
    /// The indexes of `gate` (unit build) reading `work/final`, and `notes` gated on unit
    /// build and reading `repo`, with the plan output `notes` reading `notes/final`.
    fn indexes() -> Indexes {
        Indexes {
            units: [("work", "build"), ("gate", "build"), ("notes", "notes")]
                .into_iter()
                .map(|(s, u)| (s.into(), u.into()))
                .collect(),
            tags: [
                ("work", "unit:build"),
                ("gate", "unit:build"),
                ("gate", "exit"),
            ]
            .into_iter()
            .map(|(s, t)| (s.into(), t.into()))
            .collect(),
            refs: [
                r(
                    "gate",
                    "in.items",
                    0,
                    "binding",
                    ("step", "work", "final", ""),
                ),
                r("notes", "after", 0, "gate", ("unit", "build", "", "")),
                r("notes", "in.cwd", 0, "binding", ("input", "repo", "", "")),
                r(
                    "notes",
                    "source",
                    0,
                    "output",
                    ("step", "notes", "final", ""),
                ),
            ]
            .into_iter()
            .collect(),
            edges: [
                ("work", "gate", "data", ""),
                ("gate", "notes", "gate", "build"),
            ]
            .into_iter()
            .map(|(a, b, c, d)| (a.into(), b.into(), c.into(), d.into()))
            .collect(),
        }
    }

    #[test]
    fn matching_indexes_report_nothing() {
        let mut problems = vec![];
        compare_indexes("project p", &indexes(), &indexes(), true, &mut problems);
        assert_eq!(problems, []);
    }

    #[test]
    fn a_hand_corrupted_index_row_is_reported_where_it_is() {
        let rebuilt = indexes();
        let mut stored = indexes();
        // A reference re-pointed by hand, a unit changed, a tag dropped and an edge left
        // behind by a removal that forgot it.
        stored.refs.remove(&r(
            "notes",
            "in.cwd",
            0,
            "binding",
            ("input", "repo", "", ""),
        ));
        stored.refs.insert(r(
            "notes",
            "in.cwd",
            0,
            "binding",
            ("input", "repo2", "", ""),
        ));
        stored.units.insert("notes".into(), "build".into());
        stored.tags.remove(&("gate".into(), "exit".into()));
        stored
            .edges
            .insert(("gone".into(), "notes".into(), "data".into(), String::new()));
        let mut problems = vec![];
        compare_indexes("project p", &rebuilt, &stored, true, &mut problems);
        let found: Vec<(String, String)> = problems
            .into_iter()
            .map(|p| (p.r#where, p.message))
            .collect();
        assert_eq!(
            found,
            [
                (
                    "project p: index#steps.notes.unit".to_owned(),
                    "stored unit build differs from the declaration's notes".to_owned()
                ),
                (
                    "project p: index#step_tags.gate.exit".to_owned(),
                    "declared tag missing from the index".to_owned()
                ),
                (
                    "project p: index#plan_refs.step.notes.in.cwd[0]".to_owned(),
                    "declared reference missing from the index: binding input repo".to_owned()
                ),
                (
                    "project p: index#plan_refs.step.notes.in.cwd[0]".to_owned(),
                    "stored reference the declarations do not make: binding input repo2".to_owned()
                ),
                (
                    "project p: index#plan_edges.notes".to_owned(),
                    "stored edge the declarations do not make: data notes after gone".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn edges_are_not_compared_when_the_plan_does_not_compile() {
        let rebuilt = Indexes {
            edges: Default::default(),
            ..indexes()
        };
        let mut problems = vec![];
        compare_indexes("project p", &rebuilt, &indexes(), false, &mut problems);
        assert_eq!(problems, []);
    }
}
