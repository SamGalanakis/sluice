//! Plan and result commands composed inside one writer transaction.

use crate::{Result, StoreError, WriteTransaction};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sluice_model::{
    commands::{EditResult, ProjectIdentity, RetryResult, StepSelection, StepStatus},
    edit::PreparedEdit,
    error::PublicError,
    events::Event,
    gates::{self, GateDecision, StateSnapshot, StepState},
    hash::EffectiveInput,
    ids::{
        AttemptId, ProjectId, ResultId, Revision, RunId, StepGeneration, StepId, UnitName,
        WorkGeneration,
    },
    plan::{Pause, Plan, Step, inputs_hash},
    rpc::JsonMap,
    types::{Type, check_value_at},
    units::retry_walk,
};

/// A compiled snapshot. Commands recheck its revision and document in the writer.
#[derive(Debug, Clone)]
pub struct PlanContext {
    pub project: ProjectId,
    pub revision: Revision,
    pub plan: Plan,
}

/// The message owner implements this synchronous adapter with its post command.
/// Validation checks all semantic preconditions without writes. After it succeeds,
/// post_retry may report storage failures only; the writer then rolls back.
pub trait RetryMessages {
    fn validate_retry(
        &self,
        tx: &WriteTransaction<'_>,
        project: ProjectId,
        steps: &[StepId],
        body: &str,
        author: &str,
    ) -> Result<()>;
    fn post_retry(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        project: ProjectId,
        step: &StepId,
        body: &str,
        author: &str,
    ) -> Result<()>;
}

pub(crate) fn invalid(message: impl Into<String>) -> StoreError {
    let message = message.into();
    PublicError::Invalid {
        errors: vec![message.clone()],
        message,
    }
    .into()
}
pub(crate) fn conflict(message: impl Into<String>) -> StoreError {
    PublicError::Conflict {
        message: message.into(),
        current_rev: None,
    }
    .into()
}
pub(crate) fn sql_counter(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| invalid("counter exceeds SQLite integer range"))
}
pub(crate) fn now() -> Result<String> {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| StoreError::InvalidDatabase(e.to_string()))
}
pub(crate) fn status_text(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Succeeded => "succeeded",
        StepStatus::Failed => "failed",
        StepStatus::Stale => "stale",
        StepStatus::Skipped => "skipped",
    }
}
pub(crate) fn identity(c: &Connection, project: ProjectId) -> Result<ProjectIdentity> {
    let name: Option<String> = c
        .query_row(
            "SELECT name FROM projects WHERE project_id=?1 AND deleted_at IS NULL",
            [project.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let name = name.ok_or_else(|| PublicError::NotFound {
        message: "project does not exist".into(),
    })?;
    Ok(ProjectIdentity {
        project_id: project,
        name: name
            .parse()
            .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?,
    })
}
pub(crate) fn check_context(tx: &WriteTransaction<'_>, context: &PlanContext) -> Result<()> {
    identity(tx.sql(), context.project)?;
    let (rev, doc): (i64, String) = tx.sql().query_row(
        "SELECT rev,doc FROM plans WHERE project_id=?1",
        [context.project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if Revision(rev as u64) != context.revision {
        return Err(PublicError::Conflict {
            message: format!("plan is at rev {rev}"),
            current_rev: Some(Revision(rev as u64)),
        }
        .into());
    }
    if serde_json::from_str::<JsonMap>(&doc)? != *context.plan.document() {
        return Err(conflict("compiled plan does not match stored revision"));
    }
    Ok(())
}

/// Initialize rev 1, including ordered input and step projections. Called by
/// project creation in its own transaction, never opens another connection.
pub fn initialize_plan(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    plan: &Plan,
) -> Result<()> {
    tx.sql().execute(
        "INSERT INTO plans(project_id,rev,doc) VALUES (?1,1,?2)",
        params![project.to_string(), serde_json::to_string(plan.document())?],
    )?;
    sync_projection(tx, project, plan, Revision(1))?;
    tx.changed(Some(project), "plan");
    tx.changed(Some(project), "status");
    Ok(())
}

pub fn read_state(c: &Connection, project: ProjectId) -> Result<StateSnapshot> {
    let mut state = StateSnapshot::default();
    let paused: bool = c.query_row(
        "SELECT paused FROM projects WHERE project_id=?1",
        [project.to_string()],
        |r| r.get(0),
    )?;
    state.paused = if paused { Pause::Yes } else { Pause::No };
    let mut query = c.prepare(
        "SELECT name,value FROM inputs WHERE project_id=?1 AND value IS NOT NULL ORDER BY position",
    )?;
    for row in query.query_map([project.to_string()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (name, value) = row?;
        state.inputs.0.insert(name, serde_json::from_str(&value)?);
    }
    let mut query = c.prepare("SELECT step_id,status,outputs,inputs_hash,skipped,error FROM steps WHERE project_id=?1 ORDER BY position")?;
    for row in query.query_map([project.to_string()], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })? {
        let (id, status, outputs, hash, skipped, error) = row?;
        state.steps.insert(
            id.parse()
                .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?,
            StepState {
                status: serde_json::from_value(json!(status))?,
                outputs: outputs
                    .map(|s| serde_json::from_str(&s))
                    .transpose()?
                    .unwrap_or_default(),
                inputs_hash: hash.map(|s| s.parse()).transpose()?,
                skipped: skipped
                    .map(|s| serde_json::from_str(&s))
                    .transpose()?
                    .unwrap_or_default(),
                error: error
                    .map(|s| serde_json::from_str::<PublicError>(&s).map(|e| e.to_string()))
                    .transpose()?,
                queued: vec![],
            },
        );
    }
    Ok(state)
}

pub(crate) fn wire_step(plan: &Plan, id: &StepId) -> Result<Value> {
    let doc = serde_json::to_value(plan.document())?;
    doc.get("steps")
        .and_then(|s| s.get(id.as_str()))
        .cloned()
        .ok_or_else(|| invalid("compiled step is missing its document"))
}
fn sync_projection(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    plan: &Plan,
    revision: Revision,
) -> Result<()> {
    let project = project.to_string();
    // Move positions above the entire current range to avoid UNIQUE collisions on reorder.
    let offset: i64 = tx.sql().query_row(
        "SELECT coalesce(max(position),0)+?2+1 FROM steps WHERE project_id=?1",
        params![project, plan.steps().len() as i64],
        |r| r.get(0),
    )?;
    tx.sql().execute(
        "UPDATE steps SET position=position+?2 WHERE project_id=?1",
        params![project, offset],
    )?;
    for (position, (id, step)) in plan.steps().iter().enumerate() {
        tx.sql().execute("INSERT INTO steps(project_id,step_id,position,generation,declaration,unit,paused) VALUES (?1,?2,?3,?4,?5,?6,?7)
            ON CONFLICT(project_id,step_id) DO UPDATE SET position=excluded.position,declaration=excluded.declaration,unit=excluded.unit,paused=excluded.paused",
            params![project,id.as_str(),position as i64,sql_counter(revision.0)?,wire_step(plan,id)?.to_string(),step.unit_name().to_string(),match &step.paused { Pause::No => "false".into(), Pause::Yes => "true".into(), Pause::Reason(reason) => serde_json::to_string(reason)? }])?;
    }
    let offset: i64 = tx.sql().query_row(
        "SELECT coalesce(max(position),0)+?2+1 FROM inputs WHERE project_id=?1",
        params![project, plan.inputs().len() as i64],
        |r| r.get(0),
    )?;
    tx.sql().execute(
        "UPDATE inputs SET position=position+?2 WHERE project_id=?1",
        params![project, offset],
    )?;
    for (position, (name, declaration)) in plan.inputs().iter().enumerate() {
        tx.sql().execute("INSERT INTO inputs(project_id,name,position,declaration,generation) VALUES (?1,?2,?3,?4,?5)
            ON CONFLICT(project_id,name) DO UPDATE SET position=excluded.position,declaration=excluded.declaration",
            params![project,name,position as i64,json!({"type":declaration.ty,"doc":declaration.doc}).to_string(),sql_counter(revision.0)?])?;
    }
    let names: Vec<String> = {
        let mut query = tx
            .sql()
            .prepare("SELECT name FROM inputs WHERE project_id=?1")?;
        query
            .query_map([&project], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for name in names {
        if !plan.inputs().contains_key(&name) {
            tx.sql().execute(
                "DELETE FROM inputs WHERE project_id=?1 AND name=?2",
                params![project, name],
            )?;
        }
    }
    Ok(())
}

pub fn apply_edit(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    edit: PreparedEdit,
) -> Result<EditResult> {
    let project_identity = identity(tx.sql(), project)?;
    let (rev, doc): (i64, String) = tx.sql().query_row(
        "SELECT rev,doc FROM plans WHERE project_id=?1",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if rev != sql_counter(edit.expected.0)? {
        return Err(PublicError::Conflict {
            message: format!("plan is at rev {rev}"),
            current_rev: Some(Revision(rev as u64)),
        }
        .into());
    }
    let current: Value = serde_json::from_str(&doc)?;
    let candidate = serde_json::to_value(edit.plan.document())?;
    let state = read_state(tx.sql(), project)?;
    for (id, entry) in &state.steps {
        if entry.status != StepStatus::Running {
            continue;
        }
        let old = current["steps"].get(id.as_str());
        let new = candidate["steps"].get(id.as_str());
        let strip = |value: &Value| {
            let mut value = value.clone();
            if let Some(map) = value.as_object_mut() {
                map.remove("paused");
                map.remove("tags");
            }
            value
        };
        if new.is_none() || old.map(strip) != new.map(strip) {
            return Err(invalid(format!(
                "steps.{id}: cannot change a running step, except paused and tags"
            )));
        }
    }
    let mut candidate_inputs = state.inputs.clone();
    candidate_inputs
        .0
        .retain(|name, _| edit.plan.inputs().contains_key(name));
    edit.plan
        .validate_input_values(&candidate_inputs)
        .map_err(|errors| {
            invalid(
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
    if edit.dry_run {
        return Ok(EditResult {
            project: project_identity,
            rev: Revision(rev as u64),
            preview: edit.preview,
        });
    }
    let new_rev = Revision(
        rev.checked_add(1)
            .ok_or_else(|| invalid("revision exhausted"))? as u64,
    );
    for id in state
        .steps
        .keys()
        .filter(|id| !edit.plan.steps().contains_key(*id))
    {
        archive(tx, project, id)?;
        tx.sql().execute(
            "DELETE FROM steps WHERE project_id=?1 AND step_id=?2",
            params![project.to_string(), id.as_str()],
        )?;
    }
    sync_projection(tx, project, &edit.plan, new_rev)?;
    tx.sql().execute(
        "UPDATE plans SET rev=?2,doc=?3 WHERE project_id=?1",
        params![
            project.to_string(),
            sql_counter(new_rev.0)?,
            serde_json::to_string(edit.plan.document())?
        ],
    )?;
    let author = edit.author.unwrap_or_default();
    let record = tx.append_record(
        Some(project),
        Event::PlanEdit {
            rev: new_rev,
            author: author.clone(),
            reason: edit.reason.clone(),
            ops: edit.ops.clone(),
        },
    )?;
    tx.sql().execute("INSERT INTO plan_edits(project_id,rev,seq,at,author,reason,ops) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![project.to_string(),sql_counter(new_rev.0)?,record.seq.0,record.at,author,edit.reason,serde_json::to_string(&edit.ops)?])?;
    tx.changed(Some(project), "plan");
    tx.changed(Some(project), "edits");
    reconcile(
        tx,
        &PlanContext {
            project,
            revision: new_rev,
            plan: edit.plan,
        },
    )?;
    Ok(EditResult {
        project: project_identity,
        rev: new_rev,
        preview: edit.preview,
    })
}

/// Snapshot the current terminal projection. Previously recorded declaration and
/// unit remain execution provenance even when a completed step is later retagged.
pub(crate) fn snapshot_result(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    id: &StepId,
    attempt: Option<AttemptId>,
    inputs: Option<&JsonMap>,
) -> Result<ResultId> {
    let result = ResultId::new();
    let at = now()?;
    tx.sql().execute("INSERT INTO step_results(result_id,project_id,step_id,generation,work_generation,attempt_id,unit,declaration,inputs,inputs_hash,status,outputs,error,manual,run_ids,recorded_at)
        SELECT ?3,s.project_id,s.step_id,s.generation,s.work_generation,coalesce(?4,r.attempt_id),
            CASE WHEN ?4 IS NOT NULL THEN a.unit WHEN s.manual=1 AND r.result_id IS NULL THEN s.unit ELSE coalesce(r.unit,s.unit) END,
            CASE WHEN ?4 IS NOT NULL THEN json_extract(a.request,'$.declaration') WHEN s.manual=1 AND r.result_id IS NULL THEN s.declaration ELSE coalesce(r.declaration,s.declaration) END,
            coalesce(?5,r.inputs),s.inputs_hash,s.status,s.outputs,coalesce(s.error,CASE WHEN s.skipped IS NOT NULL THEN json_object('skipped',json(s.skipped)) END),s.manual,s.run_ids,?6
        FROM steps s LEFT JOIN step_results r ON s.result_id=r.result_id LEFT JOIN attempts a ON a.attempt_id=?4 AND a.project_id=s.project_id WHERE s.project_id=?1 AND s.step_id=?2",
        params![project.to_string(),id.as_str(),result.to_string(),attempt.map(|a|a.to_string()),inputs.map(serde_json::to_string).transpose()?,at])?;
    tx.sql().execute(
        "UPDATE steps SET result_id=?3 WHERE project_id=?1 AND step_id=?2",
        params![project.to_string(), id.as_str(), result.to_string()],
    )?;
    tx.changed(Some(project), "status");
    Ok(result)
}
fn archive(tx: &mut WriteTransaction<'_>, project: ProjectId, id: &StepId) -> Result<()> {
    let (status, result): (String, Option<String>) = tx.sql().query_row(
        "SELECT status,result_id FROM steps WHERE project_id=?1 AND step_id=?2",
        params![project.to_string(), id.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let result = if matches!(
        status.as_str(),
        "succeeded" | "failed" | "stale" | "skipped"
    ) {
        Some(snapshot_result(tx, project, id, None, None)?.to_string())
    } else {
        result
    };
    if let Some(result) = result {
        tx.sql().execute(
            "UPDATE step_results SET removed_at=?2 WHERE result_id=?1",
            params![result, now()?],
        )?;
        tx.changed(Some(project), "outcomes");
    }
    Ok(())
}
pub(crate) fn status_record(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    id: &StepId,
    from: StepStatus,
    to: StepStatus,
    error: Option<PublicError>,
) -> Result<()> {
    let (runs, declaration): (String, String) = tx.sql().query_row(
        "SELECT run_ids,declaration FROM steps WHERE project_id=?1 AND step_id=?2",
        params![project.to_string(), id.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let declaration: Value = serde_json::from_str(&declaration)?;
    let needs = if to == StepStatus::Running && from != StepStatus::Running {
        serde_json::from_value(
            declaration
                .get("needs")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )?
    } else {
        JsonMap::default()
    };
    tx.append_record(
        Some(project),
        Event::StepStatus {
            step: id.clone(),
            from: Some(from),
            to,
            error,
            run_ids: serde_json::from_str(&runs)?,
            needs,
        },
    )?;
    tx.changed(Some(project), "status");
    Ok(())
}

/// Apply the pure model's gate and data-hash transitions. Never reserves work.
pub fn reconcile(tx: &mut WriteTransaction<'_>, context: &PlanContext) -> Result<()> {
    check_context(tx, context)?;
    let before = read_state(tx.sql(), context.project)?;
    let after = gates::reconcile(&context.plan, &before);
    for (id, entry) in &after.steps {
        let previous = before.status(id);
        let old = before.steps.get(id);
        if previous == entry.status
            && old.is_some_and(|old| old.skipped == entry.skipped && old.error == entry.error)
        {
            continue;
        }
        let error = entry
            .error
            .clone()
            .map(|message| PublicError::FnFailure { message });
        tx.sql().execute(
            "UPDATE steps SET status=?3,skipped=?4,error=?5 WHERE project_id=?1 AND step_id=?2",
            params![
                context.project.to_string(),
                id.as_str(),
                status_text(&entry.status),
                if entry.skipped.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&entry.skipped)?)
                },
                error.as_ref().map(serde_json::to_string).transpose()?
            ],
        )?;
        if !matches!(entry.status, StepStatus::Pending | StepStatus::Running) {
            snapshot_result(tx, context.project, id, None, None)?;
        }
        status_record(
            tx,
            context.project,
            id,
            previous,
            entry.status.clone(),
            error,
        )?;
    }
    Ok(())
}

pub fn set_input(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    name: &str,
    value: sluice_model::rpc::JsonValue,
    author: String,
    reason: String,
) -> Result<()> {
    check_context(tx, context)?;
    let declaration = context
        .plan
        .inputs()
        .get(name)
        .ok_or_else(|| invalid("no such plan input"))?;
    check_value_at(&declaration.ty, value.as_value(), &format!("inputs.{name}")).map_err(|e| {
        invalid(
            e.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    tx.sql().execute(
        "UPDATE inputs SET value=?3 WHERE project_id=?1 AND name=?2",
        params![
            context.project.to_string(),
            name,
            serde_json::to_string(&value)?
        ],
    )?;
    tx.append_record(
        Some(context.project),
        Event::PlanInput {
            rev: context.revision,
            author,
            reason,
            name: name.into(),
            value,
        },
    )?;
    tx.changed(Some(context.project), "plan");
    reconcile(tx, context)
}

pub(crate) fn validate_outputs(
    step: &Step,
    outputs: &JsonMap,
    submission: bool,
    item: bool,
) -> Result<()> {
    let declarations: Vec<_> = if submission {
        step.declared_outputs
            .iter()
            .map(|(name, decl)| (name.clone(), decl.ty.clone()))
            .collect()
    } else {
        step.signature
            .outputs
            .iter()
            .map(|(name, ty)| (name.clone(), ty.clone()))
            .chain(
                step.declared_outputs
                    .iter()
                    .map(|(name, decl)| (name.clone(), decl.ty.clone())),
            )
            .collect()
    };
    let mut errors = vec![];
    for (name, ty) in &declarations {
        let ty = if !item && step.scatter.is_some() {
            Type::List(Box::new(ty.clone()))
        } else {
            ty.clone()
        };
        match outputs.0.get(name) {
            Some(value) => {
                if let Err(e) = check_value_at(&ty, value.as_value(), &format!("outputs.{name}")) {
                    errors.extend(e.into_iter().map(|e| e.to_string()));
                }
            }
            None if matches!(ty, Type::Optional(_))
                || matches!(ty,Type::List(ref inner) if matches!(**inner,Type::Optional(_))) => {}
            None => errors.push(format!("outputs.{name}: required")),
        }
    }
    for name in outputs.0.keys() {
        if !declarations.iter().any(|(decl, _)| decl == name) {
            errors.push(format!("outputs.{name}: undeclared output"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(PublicError::Invalid {
            message: "outputs do not match declarations".into(),
            errors,
        }
        .into())
    }
}

pub fn step_set_output(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    request: sluice_model::commands::StepSetOutput,
) -> Result<ResultId> {
    check_context(tx, context)?;
    check_selector(tx.sql(), context.project, &request.project)?;
    let step = context
        .plan
        .steps()
        .get(&request.step)
        .ok_or_else(|| invalid("no such step"))?;
    let state = read_state(tx.sql(), context.project)?;
    if state.status(&request.step) == StepStatus::Running {
        return Err(invalid("cannot set outputs while running"));
    }
    validate_outputs(step, &request.outputs, false, false)?;
    if !request.force {
        let errors = match gates::evaluate_step(&context.plan, &state, step) {
            GateDecision::Ready => vec![],
            GateDecision::Wait(reasons) => reasons,
            GateDecision::Skip(reasons) => reasons.into_iter().map(|r| r.to_string()).collect(),
            GateDecision::Invalid(errors) => errors.into_iter().map(|e| e.to_string()).collect(),
        };
        if !errors.is_empty() {
            return Err(PublicError::Invalid {
                message: "step gates or inputs are not ready".into(),
                errors,
            }
            .into());
        }
    }
    let hash = inputs_hash(&context.plan, &state, step);
    tx.sql().execute("UPDATE steps SET status='succeeded',outputs=?3,inputs_hash=?4,error=NULL,skipped=NULL,manual=1,run_ids='[]',instances='{}',total=NULL,done=0,result_id=NULL WHERE project_id=?1 AND step_id=?2",
        params![context.project.to_string(),request.step.as_str(),serde_json::to_string(&request.outputs)?,hash.map(|h|h.to_string())])?;
    let inputs = effective_map(&context.plan, &state, step)?;
    let result = snapshot_result(tx, context.project, &request.step, None, inputs.as_ref())?;
    tx.append_record(
        Some(context.project),
        Event::StepOutput {
            rev: context.revision,
            author: request.author.unwrap_or_default(),
            reason: request.reason,
            step: request.step.clone(),
            outputs: request.outputs,
            force: request.force,
        },
    )?;
    status_record(
        tx,
        context.project,
        &request.step,
        state.status(&request.step),
        StepStatus::Succeeded,
        None,
    )?;
    reconcile(tx, context)?;
    Ok(result)
}
pub(crate) fn effective_map(
    plan: &Plan,
    state: &StateSnapshot,
    step: &Step,
) -> Result<Option<JsonMap>> {
    sluice_model::plan::effective_inputs(plan, state, step)
        .map(|bindings| {
            let mut map = JsonMap::default();
            for (name, value) in bindings {
                map.0.insert(
                    name,
                    match value {
                        EffectiveInput::Value(v) => v,
                        EffectiveInput::File(path) => json!({"file":path}).try_into()?,
                    },
                );
            }
            Ok(map)
        })
        .transpose()
}
pub(crate) fn check_selector(
    c: &Connection,
    project: ProjectId,
    selector: &sluice_model::ids::ProjectSelector,
) -> Result<()> {
    let current = identity(c, project)?;
    let matches = match selector {
        sluice_model::ids::ProjectSelector::Id(id) => *id == project,
        sluice_model::ids::ProjectSelector::Name(name) => *name == current.name,
    };
    if matches {
        Ok(())
    } else {
        Err(invalid("project selector does not match command context"))
    }
}
fn select(context: &PlanContext, selection: &StepSelection) -> Result<Vec<StepId>> {
    if let Some(ids) = &selection.steps {
        for id in ids {
            if !context.plan.steps().contains_key(id) {
                return Err(invalid(format!("no such step {id}")));
            }
        }
    }
    if selection.steps.as_ref().is_none_or(|ids| ids.is_empty())
        && selection.tags.as_ref().is_none_or(|tags| tags.is_empty())
    {
        return Err(invalid("select steps by ids or tags"));
    }
    Ok(context
        .plan
        .steps()
        .iter()
        .filter(|(id, step)| {
            selection.steps.as_ref().is_some_and(|ids| ids.contains(id))
                || selection
                    .tags
                    .as_ref()
                    .is_some_and(|tags| tags.iter().any(|tag| step.tags.contains(tag)))
        })
        .map(|(id, _)| id.clone())
        .collect())
}

pub fn step_retry(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    request: sluice_model::commands::StepRetry,
    messages: &mut impl RetryMessages,
) -> Result<RetryResult> {
    check_context(tx, context)?;
    check_selector(tx.sql(), context.project, &request.project)?;
    retry_selected(
        tx,
        context,
        &select(context, &request.selection)?,
        request.message.as_deref(),
        request.author.as_deref().unwrap_or_default(),
        request.reason.as_deref().unwrap_or_default(),
        messages,
    )
}
pub(crate) fn retry_selected(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    selected: &[StepId],
    message: Option<&str>,
    author: &str,
    reason: &str,
    messages: &mut impl RetryMessages,
) -> Result<RetryResult> {
    let walk = prepare_retry(tx, context, selected, message, author, messages)?;
    apply_retry(tx, context, walk, message, author, reason, messages)
}
pub(crate) fn prepare_retry(
    tx: &WriteTransaction<'_>,
    context: &PlanContext,
    selected: &[StepId],
    message: Option<&str>,
    author: &str,
    messages: &impl RetryMessages,
) -> Result<sluice_model::units::RetryWalk> {
    let state = read_state(tx.sql(), context.project)?;
    let walk = retry_walk(&context.plan, &state, selected).map_err(|e| {
        invalid(
            e.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    if let Some(body) = message {
        validate_message(body)?;
        messages.validate_retry(tx, context.project, &walk.steps, body, author)?;
    }
    Ok(walk)
}
pub(crate) fn apply_retry(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    walk: sluice_model::units::RetryWalk,
    message: Option<&str>,
    author: &str,
    reason: &str,
    messages: &mut impl RetryMessages,
) -> Result<RetryResult> {
    let state = read_state(tx.sql(), context.project)?;
    for id in walk.resets() {
        let previous = state.status(id);
        let mut retained = json!({});
        if previous == StepStatus::Failed && context.plan.steps()[id].scatter.is_some() {
            let instances: String = tx.sql().query_row(
                "SELECT instances FROM steps WHERE project_id=?1 AND step_id=?2",
                params![context.project.to_string(), id.as_str()],
                |r| r.get(0),
            )?;
            retained = serde_json::from_str(&instances)?;
            retained
                .as_object_mut()
                .ok_or_else(|| invalid("scatter instances are not an object"))?
                .retain(|_, item| item["status"] == "succeeded");
        }
        let done = retained.as_object().expect("retained object").len() as i64;
        tx.sql().execute("UPDATE steps SET status='pending',work_generation=work_generation+1,error=NULL,skipped=NULL,done=?4,instances=?3 WHERE project_id=?1 AND step_id=?2",
            params![context.project.to_string(),id.as_str(),retained.to_string(),done])?;
        let work = tx.sql().query_row(
            "SELECT work_generation FROM steps WHERE project_id=?1 AND step_id=?2",
            params![context.project.to_string(), id.as_str()],
            |r| r.get::<_, i64>(0),
        )?;
        tx.append_record(
            Some(context.project),
            Event::StepRetry {
                rev: context.revision,
                author: author.into(),
                reason: reason.into(),
                step: id.clone(),
                work: WorkGeneration(work as u64),
            },
        )?;
        status_record(tx, context.project, id, previous, StepStatus::Pending, None)?;
    }
    if let Some(body) = message {
        for id in &walk.steps {
            messages.post_retry(tx, context.project, id, body, author)?;
        }
    }
    Ok(RetryResult {
        project: identity(tx.sql(), context.project)?,
        steps: walk.steps,
        rearmed: walk.rearmed,
        stopped_at: walk.stopped_at,
    })
}
pub(crate) fn validate_message(body: &str) -> Result<()> {
    if body.trim().is_empty() || body.len() > 65536 {
        return Err(invalid("retry message must contain 1 to 65536 bytes"));
    }
    Ok(())
}

/// Cancellation records intent for admitted work. Pending outside work can fail
/// immediately because there is no process or hold to clean up.
pub fn step_cancel(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    request: sluice_model::commands::StepCancel,
) -> Result<Vec<StepId>> {
    check_context(tx, context)?;
    check_selector(tx.sql(), context.project, &request.project)?;
    let selected = select(context, &request.selection)?;
    let state = read_state(tx.sql(), context.project)?;
    for id in &selected {
        match state.status(id) {
            StepStatus::Running => {}
            StepStatus::Pending if context.plan.steps()[id].is_external() => {}
            _ => {
                return Err(invalid(format!(
                    "step {id} must be running or pending external work"
                )));
            }
        }
    }
    for id in &selected {
        tx.append_record(
            Some(context.project),
            Event::StepCancel {
                step: id.clone(),
                author: request.author.clone().unwrap_or_default(),
                reason: request.reason.clone(),
            },
        )?;
        let status = state.status(id);
        if status == StepStatus::Running {
            tx.sql().execute("UPDATE attempts SET cancel_requested=1 WHERE project_id=?1 AND step_id=?2 AND phase<>'terminal'",params![context.project.to_string(),id.as_str()])?;
        } else {
            let error = PublicError::Cancelled {
                message: request.reason.clone(),
            };
            tx.sql().execute("UPDATE steps SET status='failed',error=?3,skipped=NULL,manual=0,result_id=NULL,outputs=NULL WHERE project_id=?1 AND step_id=?2",params![context.project.to_string(),id.as_str(),serde_json::to_string(&error)?])?;
            snapshot_result(tx, context.project, id, None, None)?;
            status_record(
                tx,
                context.project,
                id,
                status,
                StepStatus::Failed,
                Some(error),
            )?;
        }
    }
    tx.changed(Some(context.project), "status");
    Ok(selected)
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResultError {
    Failure(PublicError),
    Skipped(Vec<sluice_model::types::SkipReason>),
}

/// Owned immutable result content; only removed_at is later archival metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct StepResult {
    pub id: ResultId,
    pub project: ProjectId,
    pub step: StepId,
    pub generation: StepGeneration,
    pub work: WorkGeneration,
    pub attempt: Option<AttemptId>,
    pub unit: Option<UnitName>,
    pub declaration: JsonMap,
    pub inputs: Option<JsonMap>,
    pub inputs_hash: Option<sluice_model::hash::InputsHash>,
    pub status: StepStatus,
    pub outputs: Option<JsonMap>,
    pub error: Option<ResultError>,
    pub manual: bool,
    pub run_ids: Vec<RunId>,
    pub recorded_at: String,
    pub removed_at: Option<String>,
}
pub fn read_result(c: &Connection, id: ResultId) -> Result<Option<StepResult>> {
    let row: Option<String> = c.query_row("SELECT json_object('id',result_id,'project',project_id,'step',step_id,'generation',generation,'work',work_generation,'attempt',attempt_id,'unit',unit,'declaration',json(declaration),'inputs',json(inputs),'inputs_hash',inputs_hash,'status',status,'outputs',json(outputs),'error',json(error),'manual',json(CASE WHEN manual THEN 'true' ELSE 'false' END),'run_ids',json(run_ids),'recorded_at',recorded_at,'removed_at',removed_at) FROM step_results WHERE result_id=?1",[id.to_string()],|r|r.get(0)).optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(&row)?;
    let error = if value["error"].is_null() {
        None
    } else if let Some(skipped) = value["error"].get("skipped") {
        Some(ResultError::Skipped(serde_json::from_value(
            skipped.clone(),
        )?))
    } else {
        Some(ResultError::Failure(serde_json::from_value(
            value["error"].clone(),
        )?))
    };
    Ok(Some(StepResult {
        id: serde_json::from_value(value["id"].clone())?,
        project: serde_json::from_value(value["project"].clone())?,
        step: serde_json::from_value(value["step"].clone())?,
        generation: serde_json::from_value(value["generation"].clone())?,
        work: serde_json::from_value(value["work"].clone())?,
        attempt: serde_json::from_value(value["attempt"].clone())?,
        unit: serde_json::from_value(value["unit"].clone())?,
        declaration: serde_json::from_value(value["declaration"].clone())?,
        inputs: serde_json::from_value(value["inputs"].clone())?,
        inputs_hash: serde_json::from_value(value["inputs_hash"].clone())?,
        status: serde_json::from_value(value["status"].clone())?,
        outputs: serde_json::from_value(value["outputs"].clone())?,
        error,
        manual: serde_json::from_value(value["manual"].clone())?,
        run_ids: serde_json::from_value(value["run_ids"].clone())?,
        recorded_at: serde_json::from_value(value["recorded_at"].clone())?,
        removed_at: serde_json::from_value(value["removed_at"].clone())?,
    }))
}
