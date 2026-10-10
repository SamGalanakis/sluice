//! Plan and result commands composed inside one writer transaction.

use crate::{Result, StoreError, WriteTransaction};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sluice_model::cost::{self, Counter};
use sluice_model::{
    commands::{ProjectIdentity, RetryResult, StepSelection, StepStatus},
    error::PublicError,
    events::Event,
    gates::{self, GateDecision, StateSnapshot, StepState},
    hash::EffectiveInput,
    ids::{
        AttemptId, ProjectId, RecordSeq, ResultId, Revision, RunId, StepGeneration, StepId,
        UnitName, WorkGeneration,
    },
    plan::{Pause, Plan, Step, inputs_hash},
    plan_rows::{
        CompetitorRow, ConsumerKind, EdgeKind, EdgeRow, ExportedPlan, GraphRows, HistoryRecord,
        InputRow, LeaseRow, MAX_LIMIT, OutputRow, PauseValue, PlanChange, PlanEditCommit,
        PlanHeader, PlanRows, PlanRowsError, PreparationReads, RefKind, ReferenceRow,
        ReferenceRows, ReferenceSelection, RootSection, RowSelection, ScopedState, SourceKind,
        StateEpoch, StepProjection, StepRow, StepRowView, StepRows,
    },
    rpc::JsonMap,
    types::{Type, check_value_at},
    units::retry_walk,
};

/// A compiled snapshot. Commands recheck its revision in the writer.
#[derive(Debug, Clone)]
pub struct PlanContext {
    pub project: ProjectId,
    pub revision: Revision,
    pub plan: Plan,
}

/// Store evidence for an age-filtered model prune. Prepare the model edit using
/// units(), then pass both the edit's `PlanEditCommit` and this certificate to
/// `commit_plan_edit`.
#[derive(Debug, Clone)]
pub struct PruneEligibility {
    project: ProjectId,
    revision: Revision,
    cutoff: time::OffsetDateTime,
    units: Vec<UnitName>,
    results: std::collections::BTreeMap<StepId, ResultId>,
}
impl PruneEligibility {
    pub fn units(&self) -> &[UnitName] {
        &self.units
    }
}

/// Read inside the same snapshot as the model's edit preparation. Each eligible
/// unit is done and every member's current result was recorded by the cutoff.
pub fn prune_eligible(
    c: &Connection,
    context: &PlanContext,
    cutoff: time::OffsetDateTime,
) -> Result<PruneEligibility> {
    if plan_revision(c, context.project)? != context.revision {
        return Err(conflict("plan changed before prune preparation"));
    }
    let state = read_state(c, context.project)?;
    let mut evidence = PruneEligibility {
        project: context.project,
        revision: context.revision,
        cutoff,
        units: vec![],
        results: Default::default(),
    };
    for (name, unit) in context.plan.units() {
        if !unit.done(&state) {
            continue;
        }
        let mut results = vec![];
        for step in &unit.steps {
            let Some((id, recorded_at)) = current_prune_result(c, context.project, step)? else {
                break;
            };
            if recorded_at > cutoff {
                break;
            }
            results.push((step.clone(), id));
        }
        if results.len() == unit.steps.len() {
            evidence.units.push(name.clone());
            evidence.results.extend(results);
        }
    }
    Ok(evidence)
}

fn current_prune_result(
    c: &Connection,
    project: ProjectId,
    step: &StepId,
) -> Result<Option<(ResultId, time::OffsetDateTime)>> {
    let row: Option<(String, String)> = c.query_row(
        "SELECT r.result_id,r.recorded_at FROM steps s JOIN step_results r ON r.result_id=s.result_id
         WHERE s.project_id=?1 AND s.step_id=?2",
        params![project.to_string(), step.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    row.map(|(id, at)| {
        let id = id
            .parse()
            .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?;
        let at = time::OffsetDateTime::parse(&at, &time::format_description::well_known::Rfc3339)
            .map_err(|e| StoreError::InvalidDatabase(e.to_string()))?;
        Ok((id, at))
    })
    .transpose()
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
/// The context's plan is the project's current revision: a compiled plan is certified for its
/// revision, so equal revisions mean equal plans.
pub(crate) fn check_context(tx: &WriteTransaction<'_>, context: &PlanContext) -> Result<()> {
    identity(tx.sql(), context.project)?;
    let rev = plan_revision(tx.sql(), context.project)?;
    if rev != context.revision {
        return Err(PlanRowsError::StaleRev { current: rev }.into());
    }
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
            step_state(status, outputs, hash, skipped, error)?,
        );
    }
    Ok(state)
}
/// A step row's state columns as `read_state` reads them.
fn step_state(
    status: String,
    outputs: Option<String>,
    hash: Option<String>,
    skipped: Option<String>,
    error: Option<String>,
) -> Result<StepState> {
    Ok(StepState {
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
    let changes = status_changes(&context.plan, &before);
    write_status_changes(tx, context.project, &changes)
}
/// A step whose status, skip reasons or error the model's reconcile changes.
#[derive(Debug, PartialEq)]
struct StatusChange {
    id: StepId,
    previous: StepStatus,
    entry: StepState,
}
fn status_changes(plan: &Plan, before: &StateSnapshot) -> Vec<StatusChange> {
    let after = gates::reconcile(plan, before);
    after
        .steps
        .into_iter()
        .filter_map(|(id, entry)| {
            let previous = before.status(&id);
            let old = before.steps.get(&id);
            if previous == entry.status
                && old.is_some_and(|old| old.skipped == entry.skipped && old.error == entry.error)
            {
                return None;
            }
            Some(StatusChange {
                id,
                previous,
                entry,
            })
        })
        .collect()
}
fn write_status_changes(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    changes: &[StatusChange],
) -> Result<()> {
    for StatusChange {
        id,
        previous,
        entry,
    } in changes
    {
        let error = entry
            .error
            .clone()
            .map(|message| PublicError::FnFailure { message });
        tx.sql().execute(
            "UPDATE steps SET status=?3,skipped=?4,error=?5 WHERE project_id=?1 AND step_id=?2",
            params![
                project.to_string(),
                id.as_str(),
                entry.status.as_str(),
                if entry.skipped.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&entry.skipped)?)
                },
                error.as_ref().map(serde_json::to_string).transpose()?
            ],
        )?;
        if !matches!(entry.status, StepStatus::Pending | StepStatus::Running) {
            snapshot_result(tx, project, id, None, None)?;
        }
        status_record(
            tx,
            project,
            id,
            previous.clone(),
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

/// Record failure of ready work that never created a process or acquired holds.
pub fn fail_unlaunched(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    id: &StepId,
    expected_hash: sluice_model::hash::InputsHash,
    error: PublicError,
) -> Result<()> {
    check_context(tx, context)?;
    let state = read_state(tx.sql(), context.project)?;
    let step = context
        .plan
        .steps()
        .get(id)
        .ok_or_else(|| invalid("no such step"))?;
    if state.status(id) != StepStatus::Pending
        || !matches!(
            gates::evaluate_step(&context.plan, &state, step),
            GateDecision::Ready
        )
        || inputs_hash(&context.plan, &state, step) != Some(expected_hash)
    {
        return Err(conflict("unlaunched step changed during evaluation"));
    }
    tx.sql().execute("UPDATE steps SET status='failed',error=?3,skipped=NULL,manual=0,result_id=NULL WHERE project_id=?1 AND step_id=?2", params![context.project.to_string(), id.as_str(), serde_json::to_string(&error)?])?;
    snapshot_result(tx, context.project, id, None, None)?;
    status_record(
        tx,
        context.project,
        id,
        StepStatus::Pending,
        StepStatus::Failed,
        Some(error),
    )?;
    reconcile(tx, context)
}

/// Settle scheduler-owned inline work without a process or a manual-output marker.
/// Rechecks readiness and effective inputs in the writer transaction.
pub fn settle_inline(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    expected_hash: sluice_model::hash::InputsHash,
    request: sluice_model::commands::StepSetOutput,
) -> Result<ResultId> {
    check_context(tx, context)?;
    let step = context
        .plan
        .steps()
        .get(&request.step)
        .ok_or_else(|| invalid("no such step"))?;
    let state = read_state(tx.sql(), context.project)?;
    if state.status(&request.step) != StepStatus::Pending
        || !matches!(
            gates::evaluate_step(&context.plan, &state, step),
            GateDecision::Ready
        )
    {
        return Err(conflict("inline step is no longer ready"));
    }
    validate_outputs(step, &request.outputs, false, false)?;
    let hash = inputs_hash(&context.plan, &state, step)
        .ok_or_else(|| invalid("inline inputs unavailable"))?;
    if hash != expected_hash {
        return Err(conflict("inline inputs changed during evaluation"));
    }
    let inputs = effective_map(&context.plan, &state, step)?;
    tx.sql().execute("UPDATE steps SET status='succeeded',outputs=?3,inputs_hash=?4,error=NULL,skipped=NULL,manual=0,run_ids='[]',instances='{}',total=NULL,done=0,result_id=NULL WHERE project_id=?1 AND step_id=?2",
        params![context.project.to_string(), request.step.as_str(), serde_json::to_string(&request.outputs)?, hash.to_string()])?;
    let result = snapshot_result(tx, context.project, &request.step, None, inputs.as_ref())?;
    status_record(
        tx,
        context.project,
        &request.step,
        StepStatus::Pending,
        StepStatus::Succeeded,
        None,
    )?;
    reconcile(tx, context)?;
    Ok(result)
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
        // Pauses hold back launches only; a paused step can still be given its result.
        let errors = match gates::evaluate_inputs(&context.plan, &state, step) {
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
        let record = tx.append_record(
            Some(context.project),
            Event::StepRetry {
                rev: context.revision,
                author: author.into(),
                reason: reason.into(),
                step: id.clone(),
                work: WorkGeneration(work as u64),
            },
        )?;
        mark_stopped(tx, context.project, id, "retry", author, reason, &record.at)?;
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
/// Keep who cancelled or retried `step`, and why, on its latest run of each item (`runs.stopped`
/// under `kind`, "cancel" or "retry"), so a trimmed log does not lose it. A step with no run yet
/// keeps it only in the log.
pub(crate) fn mark_stopped(
    tx: &WriteTransaction<'_>,
    project: ProjectId,
    step: &StepId,
    kind: &str,
    author: &str,
    reason: &str,
    at: &str,
) -> Result<()> {
    let entry = json!({"author": author, "reason": reason, "at": at}).to_string();
    tx.sql().execute(
        "UPDATE runs SET stopped=json_set(coalesce(stopped,'{}'),'$.'||?3,json(?4))
         WHERE project_id=?1 AND step_id=?2 AND NOT EXISTS (SELECT 1 FROM runs n
           WHERE n.project_id=runs.project_id AND n.step_id=runs.step_id AND n.item_index=runs.item_index
           AND (n.created_at>runs.created_at OR (n.created_at=runs.created_at AND n.run_id>runs.run_id)))",
        params![project.to_string(), step.as_str(), kind, entry],
    )?;
    Ok(())
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
        let author = request.author.clone().unwrap_or_default();
        let record = tx.append_record(
            Some(context.project),
            Event::StepCancel {
                step: id.clone(),
                author: author.clone(),
                reason: request.reason.clone(),
            },
        )?;
        mark_stopped(
            tx,
            context.project,
            id,
            "cancel",
            &author,
            &request.reason,
            &record.at,
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

/// Compute and freeze an age cutoff for coordinator prune preparation.
pub fn prune_eligible_age(
    sql: &Connection,
    context: &PlanContext,
    older_than_seconds: u64,
) -> Result<PruneEligibility> {
    let seconds = i64::try_from(older_than_seconds).map_err(|_| invalid("prune age too large"))?;
    let cutoff = time::OffsetDateTime::now_utc()
        .checked_sub(time::Duration::seconds(seconds))
        .ok_or_else(|| invalid("prune age too large"))?;
    prune_eligible(sql, context, cutoff)
}

// ---- schema-3 rows: reads ---------------------------------------------------------------
//
// Every read takes the caller's connection (a read snapshot, or `tx.sql()` in the writer). The
// plan is its rows; the document is assembled only by `export_plan`.

/// A store value that does not decode is a corrupt home, never a caller's mistake.
fn corrupt(error: impl std::fmt::Display) -> StoreError {
    StoreError::InvalidDatabase(error.to_string())
}
fn counter(value: i64) -> u64 {
    value.max(0) as u64
}
/// A list as SQL text for `IN (SELECT value FROM json_each(?))`, with no parameter limit.
fn json_list<T: AsRef<str>>(values: &[T]) -> String {
    serde_json::Value::Array(
        values
            .iter()
            .map(|value| Value::String(value.as_ref().to_owned()))
            .collect(),
    )
    .to_string()
}

/// The project's authored plan revision.
pub fn plan_revision(sql: &Connection, project: ProjectId) -> Result<Revision> {
    let rev: Option<i64> = sql
        .query_row(
            "SELECT rev FROM plans WHERE project_id=?1",
            [project.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    rev.map(|rev| Revision(counter(rev))).ok_or_else(|| {
        PublicError::NotFound {
            message: "project does not exist".into(),
        }
        .into()
    })
}

/// The `plans` row: revision, the present root sections in order, and the state epoch.
pub fn plan_header(sql: &Connection, project: ProjectId) -> Result<PlanHeader> {
    let row: Option<(i64, String, i64)> = sql
        .query_row(
            "SELECT rev,root_order,state_epoch FROM plans WHERE project_id=?1",
            [project.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (rev, root_order, epoch) = row.ok_or_else(|| PublicError::NotFound {
        message: "project does not exist".into(),
    })?;
    Ok(PlanHeader {
        rev: Revision(counter(rev)),
        root_order: serde_json::from_str::<Vec<RootSection>>(&root_order).map_err(corrupt)?,
        state_epoch: StateEpoch(counter(epoch)),
    })
}

/// Every authored row of the plan, each collection in position order: a whole-plan read.
pub fn read_plan_rows(sql: &Connection, project: ProjectId) -> Result<PlanRows> {
    cost::add(Counter::FullExports, 1);
    plan_rows(sql, project)
}
fn plan_rows(sql: &Connection, project: ProjectId) -> Result<PlanRows> {
    let header = plan_header(sql, project)?;
    let id = project.to_string();
    let mut decoded = 0;
    let inputs = sql
        .prepare_cached(
            "SELECT name,position,declaration FROM inputs WHERE project_id=?1 ORDER BY position",
        )?
        .query_map([&id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .map(|row| {
            let (name, position, declaration) = row?;
            decoded += 1;
            Ok(InputRow {
                name,
                position: counter(position),
                declaration: serde_json::from_str(&declaration).map_err(corrupt)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let outputs = sql
        .prepare_cached(
            "SELECT name,position,binding FROM plan_outputs WHERE project_id=?1 ORDER BY position",
        )?
        .query_map([&id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .map(|row| {
            let (name, position, binding) = row?;
            decoded += 1;
            Ok(OutputRow {
                name,
                position: counter(position),
                binding: serde_json::from_str(&binding).map_err(corrupt)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let steps = sql
        .prepare_cached(
            "SELECT step_id,position,declaration FROM steps WHERE project_id=?1
             ORDER BY position,step_id",
        )?
        .query_map([&id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .map(|row| {
            let (step, position, declaration) = row?;
            decoded += 1;
            Ok(StepRow {
                step: step.parse().map_err(corrupt)?,
                position: counter(position),
                declaration: serde_json::from_str(&declaration).map_err(corrupt)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    cost::add(Counter::DeclarationsDecoded, decoded);
    Ok(PlanRows {
        header,
        inputs,
        outputs,
        steps,
    })
}

/// The plan document assembled from its rows, never compiled: a whole-plan read.
pub fn export_plan(sql: &Connection, project: ProjectId) -> Result<ExportedPlan> {
    cost::add(Counter::FullExports, 1);
    let rows = plan_rows(sql, project)?;
    Ok(ExportedPlan {
        rev: rows.header.rev,
        document: rows.to_document(),
    })
}

/// The selected steps, `(position, id)` order. `Compact` reads only the covering indexes'
/// columns and never the declaration; `Full` reads the declaration too. `limit` reads one more
/// row to set `more`.
pub fn read_steps(
    sql: &Connection,
    project: ProjectId,
    selection: &RowSelection,
    projection: StepProjection,
) -> Result<StepRows> {
    let header = plan_header(sql, project)?;
    let (steps, more) = select_steps(sql, project, selection, projection)?;
    Ok(StepRows {
        rev: header.rev,
        state_epoch: header.state_epoch,
        steps,
        more,
    })
}
fn select_steps(
    sql: &Connection,
    project: ProjectId,
    selection: &RowSelection,
    projection: StepProjection,
) -> Result<(Vec<StepRowView>, bool)> {
    use rusqlite::types::Value as Sql;
    if [
        selection.units.as_ref().map(Vec::is_empty),
        selection.steps.as_ref().map(Vec::is_empty),
        selection.status.as_ref().map(Vec::is_empty),
    ]
    .contains(&Some(true))
    {
        return Ok((vec![], false));
    }
    let full = projection == StepProjection::Full;
    // One covering index per access path, so a compact read never touches a declaration and a
    // unit's or a status's read never scans the rest of the plan.
    let index = if selection.units.is_some() {
        "steps_unit_compact"
    } else if selection.status.is_some() {
        "steps_status_compact"
    } else {
        "steps_compact"
    };
    let mut query = format!(
        "SELECT step_id,position,run,unit,priority,paused,status{} FROM steps INDEXED BY {index}
         WHERE project_id=?",
        if full { ",declaration" } else { "" }
    );
    let mut args = vec![Sql::Text(project.to_string())];
    let mut list = |column: &str, values: Option<Vec<String>>| {
        if let Some(values) = values {
            query.push_str(&format!(
                " AND {column} IN (SELECT value FROM json_each(?))"
            ));
            args.push(Sql::Text(json_list(&values)));
        }
    };
    list(
        "unit",
        selection
            .units
            .as_ref()
            .map(|units| units.iter().map(ToString::to_string).collect()),
    );
    list(
        "step_id",
        selection
            .steps
            .as_ref()
            .map(|steps| steps.iter().map(ToString::to_string).collect()),
    );
    list(
        "status",
        selection
            .status
            .as_ref()
            .map(|status| status.iter().map(|s| s.as_str().to_owned()).collect()),
    );
    if let Some((position, step)) = &selection.after {
        query.push_str(" AND (position>? OR (position=? AND step_id>?))");
        let position = Sql::Integer(sql_counter(*position)?);
        args.extend([position.clone(), position, Sql::Text(step.to_string())]);
    }
    query.push_str(" ORDER BY position,step_id");
    if let Some(limit) = selection.limit {
        query.push_str(" LIMIT ?");
        args.push(Sql::Integer(i64::from(limit) + 1));
    }
    let mut statement = sql.prepare_cached(&query)?;
    let mut rows = statement.query(rusqlite::params_from_iter(args))?;
    let mut steps = vec![];
    while let Some(row) = rows.next()? {
        let declaration = if full {
            cost::add(Counter::DeclarationsDecoded, 1);
            Some(serde_json::from_str(&row.get::<_, String>(7)?).map_err(corrupt)?)
        } else {
            None
        };
        steps.push(StepRowView {
            step: row.get::<_, String>(0)?.parse().map_err(corrupt)?,
            position: counter(row.get(1)?),
            run: row.get(2)?,
            unit: row.get::<_, String>(3)?.parse().map_err(corrupt)?,
            priority: row.get(4)?,
            paused: serde_json::from_str::<PauseValue>(&row.get::<_, String>(5)?)
                .map_err(corrupt)?,
            status: serde_json::from_value(json!(row.get::<_, String>(6)?)).map_err(corrupt)?,
            declaration,
        });
    }
    let more = selection
        .limit
        .is_some_and(|limit| steps.len() > limit as usize);
    if let Some(limit) = selection.limit {
        steps.truncate(limit as usize);
    }
    Ok((steps, more))
}

/// The `plan_refs` rows a selection names, `(consumer, slot, ordinal)` order.
pub fn read_references(
    sql: &Connection,
    project: ProjectId,
    selection: &ReferenceSelection,
) -> Result<ReferenceRows> {
    let (condition, kind, list) = match selection {
        ReferenceSelection::Consumers(steps) => (
            "consumer_kind='step' AND consumer_id IN (SELECT value FROM json_each(?2))",
            None,
            json_list(&steps.iter().map(StepId::as_str).collect::<Vec<_>>()),
        ),
        ReferenceSelection::Outputs(names) => (
            "consumer_kind='output' AND consumer_id IN (SELECT value FROM json_each(?2))",
            None,
            json_list(names),
        ),
        ReferenceSelection::Sources { kind, ids } => (
            "source_kind=?3 AND source_id IN (SELECT value FROM json_each(?2))",
            Some(source_word(*kind)),
            json_list(ids),
        ),
    };
    let mut statement = sql.prepare_cached(&format!(
        "SELECT consumer_kind,consumer_id,slot,ordinal,kind,source_kind,source_id,source_port,
           source_path FROM plan_refs WHERE project_id=?1 AND {condition}
         ORDER BY consumer_kind,consumer_id,slot,ordinal"
    ))?;
    let project = project.to_string();
    let mut rows = match &kind {
        Some(kind) => statement.query(params![project, list, kind])?,
        None => statement.query(params![project, list])?,
    };
    let mut references = vec![];
    while let Some(row) = rows.next()? {
        references.push(reference_row(row)?);
    }
    Ok(ReferenceRows(references))
}
fn reference_row(row: &rusqlite::Row<'_>) -> Result<ReferenceRow> {
    let word = |index: usize| -> Result<Value> { Ok(Value::String(row.get(index)?)) };
    Ok(ReferenceRow {
        consumer_kind: serde_json::from_value(word(0)?).map_err(corrupt)?,
        consumer_id: row.get(1)?,
        slot: row.get(2)?,
        ordinal: u32::try_from(row.get::<_, i64>(3)?).map_err(corrupt)?,
        kind: serde_json::from_value(word(4)?).map_err(corrupt)?,
        source_kind: serde_json::from_value(word(5)?).map_err(corrupt)?,
        source_id: row.get(6)?,
        source_port: row.get(7)?,
        source_path: row.get(8)?,
    })
}

/// The selected steps (compact), every edge with a selected endpoint, and the other
/// endpoints (compact, position order) as `boundary`.
pub fn read_graph(
    sql: &Connection,
    project: ProjectId,
    selection: &RowSelection,
) -> Result<GraphRows> {
    let (steps, _) = select_steps(sql, project, selection, StepProjection::Compact)?;
    let everything = selection.units.is_none()
        && selection.steps.is_none()
        && selection.status.is_none()
        && selection.after.is_none()
        && selection.limit.is_none();
    let selected: std::collections::HashSet<&str> =
        steps.iter().map(|step| step.step.as_str()).collect();
    let project_id = project.to_string();
    let mut edges = vec![];
    {
        let mut statement;
        let mut rows = if everything {
            statement = sql.prepare_cached(
                "SELECT source_step,target_step,kind,via_unit FROM plan_edges WHERE project_id=?1
                 ORDER BY target_step,source_step,kind,via_unit",
            )?;
            statement.query([&project_id])?
        } else {
            statement = sql.prepare_cached(
                "SELECT source_step,target_step,kind,via_unit FROM plan_edges WHERE project_id=?1
                   AND target_step IN (SELECT value FROM json_each(?2))
                 UNION
                 SELECT source_step,target_step,kind,via_unit FROM plan_edges WHERE project_id=?1
                   AND source_step IN (SELECT value FROM json_each(?2))
                 ORDER BY 2,1,3,4",
            )?;
            statement.query(params![
                project_id,
                json_list(&selected.iter().collect::<Vec<_>>())
            ])?
        };
        while let Some(row) = rows.next()? {
            edges.push(edge_row(row)?);
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut outside: Vec<StepId> = vec![];
    for edge in &edges {
        for end in [&edge.source, &edge.target] {
            if !selected.contains(end.as_str()) && seen.insert(end.as_str()) {
                outside.push(end.clone());
            }
        }
    }
    let boundary = if outside.is_empty() {
        vec![]
    } else {
        select_steps(
            sql,
            project,
            &RowSelection {
                steps: Some(outside),
                ..RowSelection::default()
            },
            StepProjection::Compact,
        )?
        .0
    };
    Ok(GraphRows {
        steps,
        edges,
        boundary,
    })
}
fn edge_row(row: &rusqlite::Row<'_>) -> Result<EdgeRow> {
    let via: String = row.get(3)?;
    Ok(EdgeRow {
        source: row.get::<_, String>(0)?.parse().map_err(corrupt)?,
        target: row.get::<_, String>(1)?.parse().map_err(corrupt)?,
        kind: match row.get::<_, String>(2)?.as_str() {
            "data" => EdgeKind::Data,
            "gate" => EdgeKind::Gate,
            other => return Err(corrupt(format!("unknown edge kind {other}"))),
        },
        via_unit: if via.is_empty() {
            None
        } else {
            Some(via.parse().map_err(corrupt)?)
        },
    })
}

/// Exactly the read set of an edit's preparation (§6.4): the listed steps' state, the listed
/// inputs' values, the project's pause, the live leases on the listed resources, and each
/// listed competitor resource's pending steps (from `steps_needs`). Never a declaration, never
/// a step outside the list.
pub fn read_scoped_state(
    sql: &Connection,
    project: ProjectId,
    reads: &PreparationReads,
) -> Result<ScopedState> {
    let id = project.to_string();
    let mut read = 0;
    let mut scoped = ScopedState::default();
    let paused: bool = sql.query_row(
        "SELECT paused FROM projects WHERE project_id=?1",
        [&id],
        |r| r.get(0),
    )?;
    scoped.state.paused = if paused { Pause::Yes } else { Pause::No };
    if !reads.steps.is_empty() {
        let mut statement = sql.prepare_cached(
            "SELECT step_id,status,outputs,inputs_hash,skipped,error FROM steps
             WHERE project_id=?1 AND step_id IN (SELECT value FROM json_each(?2))
             ORDER BY position",
        )?;
        let mut rows = statement.query(params![
            id,
            json_list(&reads.steps.iter().map(StepId::as_str).collect::<Vec<_>>())
        ])?;
        while let Some(row) = rows.next()? {
            read += 1;
            scoped.state.steps.insert(
                row.get::<_, String>(0)?.parse().map_err(corrupt)?,
                step_state(
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                )?,
            );
        }
    }
    if !reads.inputs.is_empty() {
        let mut statement = sql.prepare_cached(
            "SELECT name,value FROM inputs WHERE project_id=?1 AND value IS NOT NULL
               AND name IN (SELECT value FROM json_each(?2)) ORDER BY position",
        )?;
        let mut rows = statement.query(params![id, json_list(&reads.inputs)])?;
        while let Some(row) = rows.next()? {
            read += 1;
            scoped.state.inputs.0.insert(
                row.get(0)?,
                serde_json::from_str(&row.get::<_, String>(1)?).map_err(corrupt)?,
            );
        }
    }
    if !reads.resources.is_empty() {
        let mut statement = sql.prepare_cached(
            "SELECT l.resource,l.project_id,r.step_id,l.state='held',l.amount,l.priority
             FROM leases l LEFT JOIN runs r ON r.run_id=l.run_id
             WHERE l.scope=?1 AND l.state IN ('waiting','held')
               AND l.resource IN (SELECT value FROM json_each(?2))
             ORDER BY l.resource,l.lease_id",
        )?;
        let mut rows = statement.query(params![id, json_list(&reads.resources)])?;
        while let Some(row) = rows.next()? {
            read += 1;
            scoped.leases.push(LeaseRow {
                resource: row.get(0)?,
                project: row
                    .get::<_, Option<String>>(1)?
                    .map(|p| p.parse())
                    .transpose()
                    .map_err(corrupt)?,
                step: row
                    .get::<_, Option<String>>(2)?
                    .map(|s| s.parse())
                    .transpose()
                    .map_err(corrupt)?,
                held: row.get(3)?,
                amount: counter(row.get(4)?),
                priority: row.get(5)?,
            });
        }
    }
    if !reads.competitors.is_empty() {
        let mut statement = sql.prepare_cached(
            "SELECT step_id,needs,priority FROM steps INDEXED BY steps_needs
             WHERE project_id=?1 AND status='pending' AND needs IS NOT NULL ORDER BY step_id",
        )?;
        let mut rows = statement.query([&id])?;
        while let Some(row) = rows.next()? {
            read += 1;
            let needs: JsonMap =
                serde_json::from_str(&row.get::<_, String>(1)?).map_err(corrupt)?;
            let step: StepId = row.get::<_, String>(0)?.parse().map_err(corrupt)?;
            let priority: i64 = row.get(2)?;
            for resource in &reads.competitors {
                if needs.0.contains_key(resource) {
                    scoped.competitors.push(CompetitorRow {
                        resource: resource.clone(),
                        step: step.clone(),
                        priority,
                        needs: needs.clone(),
                    });
                }
            }
        }
    }
    cost::add(Counter::StateRowsRead, read);
    Ok(scoped)
}

/// A step's declaration as written, read from its row (an attempt freezes it at reservation).
pub(crate) fn step_declaration(
    sql: &Connection,
    project: ProjectId,
    step: &StepId,
) -> Result<Value> {
    let declaration: Option<String> = sql
        .query_row(
            "SELECT declaration FROM steps WHERE project_id=?1 AND step_id=?2",
            params![project.to_string(), step.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    cost::add(Counter::DeclarationsDecoded, 1);
    serde_json::from_str(&declaration.ok_or_else(|| invalid("no such step"))?).map_err(corrupt)
}

// ---- schema-3 rows: the edit commit -----------------------------------------------------

/// What `commit_plan_edit` did: committed at this revision (the current one when the edit
/// changes nothing), or found a token moved since preparation and wrote nothing, so the
/// coordinator prepares the edit again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    Committed(Revision),
    Stale,
}

/// The `paused`, `run`, `priority` and `needs` columns, each written with its `CHECK`'s own
/// expression of the declaration (parameter 1), so no write can let them drift.
const HELD_COLUMNS: &str = "CASE json_type(?1,'$.paused') WHEN 'true' THEN 'true'
     WHEN 'text' THEN json_quote(json_extract(?1,'$.paused')) ELSE 'false' END,
   json_extract(?1,'$.run'), coalesce(json_extract(?1,'$.priority'),0), json_extract(?1,'$.needs')";

/// Commit a prepared plan edit (§4) in the writer: the project is live and the home admits
/// plan edits; the caller's `rev` is current (else `conflict`); the tokens still hold (else
/// `Stale`, nothing written); an empty change set writes nothing. Then removed steps are
/// archived and deleted, the row changes and index rows written, the status transitions
/// applied, and the `plan.edit` record and history row appended at the next revision.
pub fn commit_plan_edit(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    commit: &PlanEditCommit,
    prune: Option<&PruneEligibility>,
) -> Result<CommitOutcome> {
    identity(tx.sql(), project)?;
    let mode: String =
        tx.sql()
            .query_row("SELECT mode FROM maintenance WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
    if mode != "normal" {
        return Err(PublicError::Busy {
            message: format!("{mode} rejects new plan work and user calls"),
            retryable: false,
        }
        .into());
    }
    let id = project.to_string();
    let header = plan_header(tx.sql(), project)?;
    let board_rev: i64 = tx.sql().query_row(
        "SELECT board_rev FROM projects WHERE project_id=?1",
        [&id],
        |r| r.get(0),
    )?;
    if let Some(rev) = commit.rev
        && rev != header.rev
    {
        return Err(PlanRowsError::StaleRev {
            current: header.rev,
        }
        .into());
    }
    let tokens = &commit.tokens;
    if tokens.plan_rev != header.rev
        || tokens.state_epoch != header.state_epoch
        || tokens.board_rev != Revision(counter(board_rev))
    {
        return Ok(CommitOutcome::Stale);
    }
    if commit.rows.changes.is_empty() {
        return Ok(CommitOutcome::Committed(header.rev));
    }
    check_prune(tx, project, header.rev, commit, prune)?;
    check_commit(tx.sql(), project, commit)?;
    let revision = Revision(header.rev.0 + 1);
    let generation = sql_counter(revision.0)?;
    let mut written = 0u64;
    // 4: removed steps: outcome archived, row deleted (tags and edges by cascade, its
    // references by trigger).
    for step in &commit.state.removed {
        archive(tx, project, step)?;
        written += tx.sql().execute(
            "DELETE FROM steps WHERE project_id=?1 AND step_id=?2",
            params![id, step.as_str()],
        )? as u64;
    }
    // 5: the row changes, deletes first; a put that moves a row moves it above the
    // collection's maximum first, so no two rows share a position mid-transaction.
    let mut inputs = vec![];
    let mut outputs = vec![];
    let mut steps = vec![];
    for change in &commit.rows.changes {
        match change {
            PlanChange::HeaderPut { root_order } => {
                written += tx.sql().execute(
                    "UPDATE plans SET root_order=?2 WHERE project_id=?1",
                    params![id, serde_json::to_string(root_order)?],
                )? as u64;
            }
            PlanChange::InputDelete { name } => {
                written += tx.sql().execute(
                    "DELETE FROM inputs WHERE project_id=?1 AND name=?2",
                    params![id, name],
                )? as u64;
            }
            PlanChange::OutputDelete { name } => {
                written += tx.sql().execute(
                    "DELETE FROM plan_outputs WHERE project_id=?1 AND name=?2",
                    params![id, name],
                )? as u64;
            }
            PlanChange::StepDelete { .. } => {}
            PlanChange::InputPut {
                name,
                position,
                declaration,
            } => inputs.push((
                name.as_str(),
                *position,
                serde_json::to_string(declaration)?,
            )),
            PlanChange::OutputPut {
                name,
                position,
                binding,
            } => outputs.push((name.as_str(), *position, serde_json::to_string(binding)?)),
            PlanChange::StepPut {
                step,
                position,
                declaration,
            } => steps.push((
                step.as_str(),
                *position,
                serde_json::to_string(declaration)?,
            )),
        }
    }
    cost::add(
        Counter::DeclarationsWritten,
        (inputs.len() + outputs.len() + steps.len()) as u64,
    );
    let units: std::collections::HashMap<&str, &str> = commit
        .rows
        .step_index
        .iter()
        .map(|index| (index.step.as_str(), index.unit.as_str()))
        .collect();
    written += put_rows(tx, project, Collection::Inputs, &inputs, generation, &units)?;
    written += put_rows(
        tx,
        project,
        Collection::Outputs,
        &outputs,
        generation,
        &units,
    )?;
    written += put_rows(tx, project, Collection::Steps, &steps, generation, &units)?;
    // 6: the index rows the changes imply.
    for index in &commit.rows.step_index {
        written += tx.sql().execute(
            "UPDATE steps SET unit=?3 WHERE project_id=?1 AND step_id=?2 AND unit IS NOT ?3",
            params![id, index.step.as_str(), index.unit.as_str()],
        )? as u64;
        written += tx.sql().execute(
            "DELETE FROM step_tags WHERE project_id=?1 AND step_id=?2",
            params![id, index.step.as_str()],
        )? as u64;
        for tag in &index.tags {
            written += tx.sql().execute(
                "INSERT INTO step_tags(project_id,step_id,tag) VALUES (?1,?2,?3)",
                params![id, index.step.as_str(), tag],
            )? as u64;
        }
        written += replace_references(tx, project, "step", index.step.as_str(), &index.references)?;
    }
    for (name, references) in &commit.rows.output_refs {
        written += replace_references(tx, project, "output", name, references)?;
    }
    for (target, edges) in &commit.rows.edges {
        written += tx.sql().execute(
            "DELETE FROM plan_edges WHERE project_id=?1 AND target_step=?2",
            params![id, target.as_str()],
        )? as u64;
        for edge in edges {
            written += insert_edge(tx.sql(), project, edge)?;
        }
    }
    // 7: the status transitions the edit's reconciliation settled.
    let changes: Vec<StatusChange> = commit
        .state
        .transitions
        .iter()
        .map(|transition| StatusChange {
            id: transition.step.clone(),
            previous: transition.from.clone(),
            entry: StepState {
                status: transition.to.clone(),
                skipped: transition.skipped.clone(),
                error: transition.error.clone(),
                ..StepState::default()
            },
        })
        .collect();
    write_status_changes(tx, project, &changes)?;
    // 8, 9: the record, the history row, the revision.
    let record = tx.append_record(
        Some(project),
        Event::PlanEdit {
            rev: revision,
            author: commit.author.clone(),
            reason: commit.reason.clone(),
            changes: commit.rows.changes.clone(),
        },
    )?;
    written += tx.sql().execute(
        "INSERT INTO plan_edits(project_id,rev,seq,at,author,reason,changes) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            id,
            generation,
            record.seq.0,
            record.at,
            commit.author,
            commit.reason,
            serde_json::to_string(&commit.rows.changes)?
        ],
    )? as u64;
    tx.sql().execute(
        "UPDATE plans SET rev=?2 WHERE project_id=?1",
        params![id, generation],
    )?;
    cost::add(Counter::RowsWritten, written);
    tx.changed(Some(project), "plan");
    tx.changed(Some(project), "edits");
    if !commit.state.removed.is_empty() || !changes.is_empty() {
        tx.changed(Some(project), "status");
    }
    Ok(CommitOutcome::Committed(revision))
}

/// A prune commits only with the store's age evidence for its own revision, and only while
/// every member's result is still the one the evidence froze at the cutoff.
fn check_prune(
    tx: &WriteTransaction<'_>,
    project: ProjectId,
    rev: Revision,
    commit: &PlanEditCommit,
    evidence: Option<&PruneEligibility>,
) -> Result<()> {
    let (prune, evidence) = match (&commit.prune, evidence) {
        (None, None) => return Ok(()),
        (Some(prune), Some(evidence)) => (prune, evidence),
        (None, Some(_)) => {
            return Err(invalid(
                "prune evidence was given for an edit that is not a prune",
            ));
        }
        (Some(_), None) => {
            return Err(invalid(
                "a prune commits only with the store's age evidence",
            ));
        }
    };
    if evidence.project != project
        || evidence.revision != rev
        || prune
            .units
            .iter()
            .any(|unit| !evidence.units.contains(unit))
    {
        return Err(conflict(
            "prune eligibility does not match the prepared edit",
        ));
    }
    for step in &prune.steps {
        let current = current_prune_result(tx.sql(), project, step)?;
        if current
            .is_none_or(|(id, at)| evidence.results.get(step) != Some(&id) || at > evidence.cutoff)
        {
            return Err(conflict(format!(
                "steps.{step}: prune result changed; prepare prune again"
            )));
        }
    }
    Ok(())
}

/// The commit's parts agree: its step deletes are the steps it removes, its puts of steps
/// that do not exist are the steps it adds, and every step it puts has its index rows.
fn check_commit(sql: &Connection, project: ProjectId, commit: &PlanEditCommit) -> Result<()> {
    use std::collections::BTreeSet;
    let deleted: BTreeSet<&str> = commit
        .rows
        .changes
        .iter()
        .filter_map(|change| match change {
            PlanChange::StepDelete { step } => Some(step.as_str()),
            _ => None,
        })
        .collect();
    let removed: BTreeSet<&str> = commit.state.removed.iter().map(StepId::as_str).collect();
    let indexed: BTreeSet<&str> = commit
        .rows
        .step_index
        .iter()
        .map(|index| index.step.as_str())
        .collect();
    let mut added = BTreeSet::new();
    let mut statement =
        sql.prepare_cached("SELECT 1 FROM steps WHERE project_id=?1 AND step_id=?2")?;
    for change in &commit.rows.changes {
        if let PlanChange::StepPut { step, .. } = change {
            if !indexed.contains(step.as_str()) {
                return Err(invalid(format!(
                    "plan edit commit is inconsistent: steps.{step} is put without its index rows"
                )));
            }
            if !statement.exists(params![project.to_string(), step.as_str()])? {
                added.insert(step.as_str());
            }
        }
    }
    let declared: BTreeSet<&str> = commit.state.added.iter().map(StepId::as_str).collect();
    if deleted != removed || added != declared {
        return Err(invalid(
            "plan edit commit is inconsistent: its step deletes and new steps must be its removed and added steps",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Collection {
    Inputs,
    Outputs,
    Steps,
}

/// Insert or update one collection's puts, `(key, position, declaration text)`. Rows that
/// move go above the collection's maximum (and every put's position) first.
fn put_rows(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    collection: Collection,
    puts: &[(&str, u64, String)],
    generation: i64,
    units: &std::collections::HashMap<&str, &str>,
) -> Result<u64> {
    if puts.is_empty() {
        return Ok(0);
    }
    let (table, key) = match collection {
        Collection::Inputs => ("inputs", "name"),
        Collection::Outputs => ("plan_outputs", "name"),
        Collection::Steps => ("steps", "step_id"),
    };
    let id = project.to_string();
    let mut current = Vec::with_capacity(puts.len());
    {
        let mut statement = tx.sql().prepare_cached(&format!(
            "SELECT position FROM {table} WHERE project_id=?1 AND {key}=?2"
        ))?;
        for (name, _, _) in puts {
            current.push(
                statement
                    .query_row(params![id, name], |r| r.get::<_, i64>(0))
                    .optional()?,
            );
        }
    }
    let mut written = 0;
    let moving: Vec<&str> = puts
        .iter()
        .zip(&current)
        .filter(|((_, position, _), now)| {
            now.is_some_and(|now| Some(now) != sql_counter(*position).ok())
        })
        .map(|((name, _, _), _)| *name)
        .collect();
    if !moving.is_empty() {
        cost::add(Counter::PositionsRenumbered, moving.len() as u64);
        // Above every current row and every position a put takes, so a moved row never
        // meets a row put after it.
        let highest = puts
            .iter()
            .map(|(_, position, _)| *position)
            .max()
            .unwrap_or(0);
        let above: i64 = tx.sql().query_row(
            &format!(
                "SELECT max(coalesce(max(position),-1),?2)+1 FROM {table} WHERE project_id=?1"
            ),
            params![id, sql_counter(highest)?],
            |r| r.get(0),
        )?;
        let mut shift = tx.sql().prepare_cached(&format!(
            "UPDATE {table} SET position=?3 WHERE project_id=?1 AND {key}=?2"
        ))?;
        for (offset, name) in moving.iter().enumerate() {
            written += shift.execute(params![id, name, above + offset as i64])? as u64;
        }
    }
    for ((name, position, declaration), now) in puts.iter().zip(&current) {
        let position = sql_counter(*position)?;
        written += match (collection, now) {
            (Collection::Inputs, Some(_)) => tx.sql().execute(
                "UPDATE inputs SET position=?3,declaration=?4 WHERE project_id=?1 AND name=?2",
                params![id, name, position, declaration],
            )?,
            (Collection::Inputs, None) => tx.sql().execute(
                "INSERT INTO inputs(project_id,name,position,declaration,generation) VALUES (?1,?2,?3,?4,?5)",
                params![id, name, position, declaration, generation],
            )?,
            (Collection::Outputs, Some(_)) => tx.sql().execute(
                "UPDATE plan_outputs SET position=?3,binding=?4 WHERE project_id=?1 AND name=?2",
                params![id, name, position, declaration],
            )?,
            (Collection::Outputs, None) => tx.sql().execute(
                "INSERT INTO plan_outputs(project_id,name,position,binding) VALUES (?1,?2,?3,?4)",
                params![id, name, position, declaration],
            )?,
            (Collection::Steps, Some(_)) => tx.sql().execute(
                &format!(
                    "UPDATE steps SET (position,declaration,paused,run,priority,needs)=(?4,?1,{HELD_COLUMNS})
                     WHERE project_id=?2 AND step_id=?3"
                ),
                params![declaration, id, name, position],
            )?,
            (Collection::Steps, None) => {
                let unit = units.get(name).ok_or_else(|| {
                    invalid(format!("plan edit commit is inconsistent: steps.{name} has no unit"))
                })?;
                tx.sql().execute(
                    &format!(
                        "INSERT INTO steps(declaration,project_id,step_id,position,generation,unit,paused,run,priority,needs)
                         VALUES (?1,?2,?3,?4,?5,?6,{HELD_COLUMNS})"
                    ),
                    params![declaration, id, name, position, generation, unit],
                )?
            }
        } as u64;
    }
    Ok(written)
}

fn replace_references(
    tx: &WriteTransaction<'_>,
    project: ProjectId,
    consumer_kind: &str,
    consumer: &str,
    references: &[ReferenceRow],
) -> Result<u64> {
    let id = project.to_string();
    let mut written = tx.sql().execute(
        "DELETE FROM plan_refs WHERE project_id=?1 AND consumer_kind=?2 AND consumer_id=?3",
        params![id, consumer_kind, consumer],
    )? as u64;
    for reference in references {
        written += insert_reference(tx.sql(), project, reference)?;
    }
    Ok(written)
}
pub(crate) fn insert_reference(
    sql: &Connection,
    project: ProjectId,
    reference: &ReferenceRow,
) -> Result<u64> {
    Ok(sql
        .prepare_cached(
            "INSERT INTO plan_refs(project_id,consumer_kind,consumer_id,slot,ordinal,kind,
               source_kind,source_id,source_port,source_path) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        )?
        .execute(params![
            project.to_string(),
            consumer_word(reference.consumer_kind),
            reference.consumer_id,
            reference.slot,
            i64::from(reference.ordinal),
            ref_word(reference.kind),
            source_word(reference.source_kind),
            reference.source_id,
            reference.source_port,
            reference.source_path
        ])? as u64)
}
pub(crate) fn insert_edge(sql: &Connection, project: ProjectId, edge: &EdgeRow) -> Result<u64> {
    Ok(sql
        .prepare_cached(
            "INSERT INTO plan_edges(project_id,source_step,target_step,kind,via_unit) VALUES (?1,?2,?3,?4,?5)",
        )?
        .execute(params![
            project.to_string(),
            edge.source.as_str(),
            edge.target.as_str(),
            match edge.kind {
                EdgeKind::Data => "data",
                EdgeKind::Gate => "gate",
            },
            edge.via_unit.as_ref().map_or("", |unit| unit.as_str())
        ])? as u64)
}
/// The words `plan_refs` stores for its enum columns.
fn consumer_word(kind: ConsumerKind) -> &'static str {
    match kind {
        ConsumerKind::Step => "step",
        ConsumerKind::Output => "output",
    }
}
fn ref_word(kind: RefKind) -> &'static str {
    match kind {
        RefKind::Binding => "binding",
        RefKind::Gate => "gate",
        RefKind::Output => "output",
    }
}
fn source_word(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Step => "step",
        SourceKind::Input => "input",
        SourceKind::Unit => "unit",
    }
}

// ---- history ------------------------------------------------------------------------------

/// A page of the plan's history (§5.4): every plan edit (from `plan_edits`, never trimmed)
/// and the log's retained `plan.input`, `step.output` and `step.retry` records, merged by
/// `seq`, oldest first. `since_rev` keeps entries whose `rev` is greater, `after_seq` those
/// whose `seq` is greater. `limit` is 1 to 1000 (larger reads as 1000; 0 is `bad_request`).
/// The second value is the last entry's seq when more entries match.
pub fn history(
    sql: &Connection,
    project: ProjectId,
    since_rev: Option<Revision>,
    after_seq: Option<RecordSeq>,
    limit: u32,
) -> Result<(Vec<HistoryRecord>, Option<RecordSeq>)> {
    if limit == 0 {
        return Err(PlanRowsError::Limit.into());
    }
    let limit = limit.min(MAX_LIMIT);
    let mut statement = sql.prepare_cached(
        "SELECT seq,at,payload,payload_version FROM (
           SELECT seq,at,payload,payload_version FROM records WHERE project_id=?1
             AND kind IN ('plan.input','step.output','step.retry')
           UNION ALL
           SELECT seq,at,json_object('kind','plan.edit','rev',rev,'author',author,
             'reason',reason,'changes',json(changes)),?5 FROM plan_edits WHERE project_id=?1
         ) WHERE (?2 IS NULL OR json_extract(payload,'$.rev')>?2) AND (?3 IS NULL OR seq>?3)
         ORDER BY seq LIMIT ?4",
    )?;
    let mut rows = statement.query(params![
        project.to_string(),
        since_rev.map(|rev| sql_counter(rev.0)).transpose()?,
        after_seq.map(|seq| seq.0),
        i64::from(limit) + 1,
        crate::schema::RECORD_PAYLOAD_VERSION
    ])?;
    let mut entries = vec![];
    while let Some(row) = rows.next()? {
        let version: i64 = row.get(3)?;
        if version != crate::schema::RECORD_PAYLOAD_VERSION {
            return Err(StoreError::InvalidDatabase(format!(
                "unsupported record payload version {version}"
            )));
        }
        entries.push(HistoryRecord {
            seq: RecordSeq(row.get(0)?),
            at: row.get(1)?,
            project: Some(project),
            event: serde_json::from_str(&row.get::<_, String>(2)?).map_err(corrupt)?,
        });
    }
    let more = entries.len() > limit as usize;
    entries.truncate(limit as usize);
    let next = more
        .then(|| entries.last().map(|entry| entry.seq))
        .flatten();
    Ok((entries, next))
}
