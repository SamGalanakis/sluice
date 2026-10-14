//! Bypass presets (SPEC §7.5 `step_bypass`): a pending step's stored outputs, who set them and
//! why, in the additive `steps.bypass` column. Plan reconciliation applies a preset once the
//! step's gates and inputs are ready: the step settles as succeeded with the preset's outputs,
//! manual, as `step_set_output` would, in the transaction that made it ready, and is never
//! launched. A preset is runtime state: it is no part of the step's declaration, so neither the
//! plan export nor a pinned release's compiled plan sees it.

use crate::{
    Result, WriteTransaction, identities, messages,
    plans::{
        PlanContext, check_context, check_selector, effective_map, identity, invalid, now,
        read_state, reconcile, snapshot_result, status_record, validate_outputs,
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sluice_model::{
    commands::{StepBypass, StepStatus},
    error::PublicError,
    events::Event,
    gates::{self, GateDecision},
    identity::SessionName,
    ids::{ProjectId, ProjectSelector, ResultId, StepId},
    plan::{Plan, Step, inputs_hash},
    rpc::{JsonMap, JsonValue},
};
use std::collections::BTreeMap;

/// A step's preset as `steps.bypass` stores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub outputs: JsonMap,
    pub author: String,
    pub reason: String,
    /// When it was set (RFC 3339).
    pub at: String,
    /// Set once the runner has applied it; a preset is applied at most once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<Applied>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Applied {
    pub at: String,
    /// The result it settled the step with.
    pub result: ResultId,
}
impl Preset {
    /// Waiting to be applied.
    pub fn pending(&self) -> bool {
        self.applied.is_none()
    }
    /// What the reply and `status` show of a pending preset.
    pub fn summary(&self) -> Value {
        json!({"outputs": self.outputs, "author": self.author, "reason": self.reason, "at": self.at})
    }
}

/// Check outputs against the step's declared outputs (its fn's and its own), as
/// `step_set_output` checks them: every required one, the types, nothing undeclared.
pub fn check_outputs(step: &Step, outputs: &JsonMap) -> Result<()> {
    validate_outputs(step, outputs, false, false)
}

/// Every preset of the project's steps, applied ones included, by step.
pub fn presets(c: &Connection, project: ProjectId) -> Result<BTreeMap<StepId, Preset>> {
    let mut query = c.prepare_cached(
        "SELECT step_id,bypass FROM steps WHERE project_id=?1 AND bypass IS NOT NULL",
    )?;
    let rows = query
        .query_map([project.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(id, text)| Ok((parse_step(&id)?, serde_json::from_str(&text)?)))
        .collect()
}
/// One step's preset, applied or not.
pub fn preset(c: &Connection, project: ProjectId, step: &StepId) -> Result<Option<Preset>> {
    let text: Option<Option<String>> = c
        .query_row(
            "SELECT bypass FROM steps WHERE project_id=?1 AND step_id=?2",
            params![project.to_string(), step.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    text.flatten()
        .map(|text| serde_json::from_str(&text).map_err(Into::into))
        .transpose()
}
/// Whether the step's current result is the one its preset settled it with: it reads as
/// bypassed, not set by hand.
pub fn bypassed(c: &Connection, project: ProjectId, step: &StepId) -> Result<bool> {
    let result: Option<Option<String>> = c
        .query_row(
            "SELECT result_id FROM steps WHERE project_id=?1 AND step_id=?2",
            params![project.to_string(), step.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(result) = result.flatten() else {
        return Ok(false);
    };
    Ok(preset(c, project, step)?
        .and_then(|p| p.applied)
        .is_some_and(|a| a.result.to_string() == result))
}

/// How a preset marks its step on the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// A pending step with a preset waiting, set by this author.
    Waiting(String),
    /// Its current result is the one its preset settled it with.
    Bypassed,
}
/// Each of the project's steps a preset marks now, read in one query.
pub fn marks(c: &Connection, project: ProjectId) -> Result<BTreeMap<StepId, Mark>> {
    let mut query = c.prepare_cached(
        "SELECT step_id,bypass,result_id,status FROM steps WHERE project_id=?1 AND bypass IS NOT NULL",
    )?;
    let rows = query
        .query_map([project.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut marks = BTreeMap::new();
    for (id, text, result, status) in rows {
        let preset: Preset = serde_json::from_str(&text)?;
        let mark = match preset.applied {
            None if status == "pending" => Mark::Waiting(preset.author),
            Some(applied) if result.is_some_and(|r| r == applied.result.to_string()) => {
                Mark::Bypassed
            }
            _ => continue,
        };
        marks.insert(parse_step(&id)?, mark);
    }
    Ok(marks)
}

fn parse_step(id: &str) -> Result<StepId> {
    id.parse()
        .map_err(|e| crate::StoreError::InvalidDatabase(format!("{e}")))
}

/// `step_bypass`: set (or with `off` clear) a pending step's preset, record it, and apply it at
/// once when the step is already ready. Returns `{project, step, status, bypass, applied}`:
/// `bypass` is the preset still waiting (null once cleared or applied).
pub fn step_bypass(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    request: StepBypass,
) -> Result<Value> {
    check_context(tx, context)?;
    check_selector(tx.sql(), context.project, &request.project)?;
    let id = request.step;
    let step = context
        .plan
        .steps()
        .get(&id)
        .ok_or_else(|| invalid(format!("no such step {id}")))?;
    let state = read_state(tx.sql(), context.project)?;
    let status = state.status(&id);
    if status != StepStatus::Pending {
        return Err(not_pending(&id, &status, request.off));
    }
    if request.reason.trim().is_empty() {
        return Err(invalid("reason: required"));
    }
    let author = request.author.unwrap_or_default();
    let current = preset(tx.sql(), context.project, &id)?.filter(Preset::pending);
    if request.off {
        if request.outputs.is_some() {
            return Err(invalid("outputs: not taken with off"));
        }
        if current.is_none() {
            return Err(invalid(format!("step {id} has no bypass to clear")));
        }
        tx.sql().execute(
            "UPDATE steps SET bypass=NULL WHERE project_id=?1 AND step_id=?2",
            params![context.project.to_string(), id.as_str()],
        )?;
        record(tx, context, &id, None, author, request.reason)?;
    } else {
        let outputs = request.outputs.unwrap_or_default();
        check_outputs(step, &outputs)?;
        set(tx, context, &id, outputs, author, request.reason)?;
    }
    reconcile(tx, context)?;
    let status = read_state(tx.sql(), context.project)?.status(&id);
    let waiting = preset(tx.sql(), context.project, &id)?.filter(Preset::pending);
    Ok(json!({
        "project": identity(tx.sql(), context.project)?,
        "step": id,
        "status": status,
        "bypass": waiting.as_ref().map(Preset::summary),
        "applied": !request.off && waiting.is_none(),
    }))
}

/// The refusal of a bypass on a step that is not pending, naming the verbs that apply to it.
fn not_pending(id: &StepId, status: &StepStatus, off: bool) -> crate::StoreError {
    let verbs = match status {
        StepStatus::Running => {
            "step_cancel stops it (step_settle settles a finishing one), then step_set_output gives it outputs"
        }
        StepStatus::Succeeded => "step_set_output replaces its outputs; step_retry runs it again",
        StepStatus::Failed | StepStatus::Stale | StepStatus::Skipped => {
            "step_set_output gives it outputs; step_retry makes it pending again"
        }
        StepStatus::Pending => "",
    };
    let what = if off { "clears" } else { "bypasses" };
    invalid(format!(
        "step {id} is {}: step_bypass {what} a pending step only; {verbs}",
        status.as_str()
    ))
}

/// Store presets on new pending steps (`unit_add`'s `bypass`), already checked against their
/// steps, record each, and apply any whose step is already ready.
pub fn set_presets(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    presets: Vec<(StepId, JsonMap)>,
    author: &str,
    reason: &str,
) -> Result<()> {
    if presets.is_empty() {
        return Ok(());
    }
    check_context(tx, context)?;
    for (id, outputs) in presets {
        set(tx, context, &id, outputs, author.into(), reason.into())?;
    }
    reconcile(tx, context)
}

fn set(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    id: &StepId,
    outputs: JsonMap,
    author: String,
    reason: String,
) -> Result<()> {
    let preset = Preset {
        outputs: outputs.clone(),
        author: author.clone(),
        reason: reason.clone(),
        at: now()?,
        applied: None,
    };
    tx.sql().execute(
        "UPDATE steps SET bypass=?3 WHERE project_id=?1 AND step_id=?2",
        params![
            context.project.to_string(),
            id.as_str(),
            serde_json::to_string(&preset)?
        ],
    )?;
    record(tx, context, id, Some(outputs), author, reason)
}

/// Note a set or clear in the log as a `say` from `sluice` whose `data.step_bypass` holds
/// `{step, action: "set"|"clear", by, reason, rev, outputs?}`, as a unit handoff's note is: an existing record kind, so
/// a release pinned before `step_bypass` reads the log unchanged. It goes to the author when
/// that is an @session (registering it, as an edit would), else to `owner`; a say wakes
/// nobody, and it is never addressed to the step, whose next run would read it.
fn record(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    id: &StepId,
    outputs: Option<JsonMap>,
    author: String,
    reason: String,
) -> Result<()> {
    let to = match SessionName::parse_as("author", &author) {
        Ok(me) => {
            identities::register(tx, context.project, &me)?;
            me.to_string()
        }
        Err(_) => messages::OWNER_STREAM.to_owned(),
    };
    let because = if reason.is_empty() {
        String::new()
    } else {
        format!(" ({reason})")
    };
    let body = match &outputs {
        Some(outputs) => format!(
            "{author} bypassed step {id}{because}. Once ready it settles succeeded with {} instead of running.",
            serde_json::to_string(outputs)?
        ),
        None => format!("{author} cleared step {id}'s bypass{because}. It runs as usual."),
    };
    let mut note = json!({
        "step": id.as_str(),
        "action": if outputs.is_some() { "set" } else { "clear" },
        "by": author,
        "reason": reason,
        "rev": context.revision,
    });
    if let Some(outputs) = outputs {
        note["outputs"] = serde_json::to_value(outputs)?;
    }
    messages::post(
        tx,
        messages::Post {
            project: ProjectSelector::Id(context.project),
            speaker: messages::Speaker::Sluice,
            body,
            verb: messages::Verb::Say {
                to,
                data: Some(JsonValue::try_from(json!({ "step_bypass": note }))?),
            },
        },
        &messages::NoPlanInputs,
    )?;
    tx.changed(Some(context.project), "status");
    Ok(())
}

/// Apply every waiting preset whose step is pending with its gates and inputs ready (pauses do
/// not hold it back: no run starts). Answers whether any was applied, so reconciliation runs
/// again for what that settles. A preset whose outputs no longer fit the step (its
/// declaration changed since) fails the step instead, saying so; nothing launches either way.
pub(crate) fn apply_ready(tx: &mut WriteTransaction<'_>, context: &PlanContext) -> Result<bool> {
    let waiting: Vec<(String, String)> = tx
        .sql()
        .prepare_cached(
            "SELECT step_id,bypass FROM steps WHERE project_id=?1 AND status='pending'
             AND bypass IS NOT NULL AND json_type(bypass,'$.applied') IS NULL ORDER BY position",
        )?
        .query_map([context.project.to_string()], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if waiting.is_empty() {
        return Ok(false);
    }
    let state = read_state(tx.sql(), context.project)?;
    let mut applied = false;
    for (id, text) in waiting {
        let id = parse_step(&id)?;
        let Some(step) = context.plan.steps().get(&id) else {
            continue;
        };
        if !matches!(
            gates::evaluate_inputs(&context.plan, &state, step),
            GateDecision::Ready
        ) {
            continue;
        }
        let preset: Preset = serde_json::from_str(&text)?;
        let result = settle(tx, context, &context.plan, &state, step, &preset)?;
        let applied_at = serde_json::to_string(&Applied { at: now()?, result })?;
        tx.sql().execute(
            "UPDATE steps SET bypass=json_set(bypass,'$.applied',json(?3)) WHERE project_id=?1 AND step_id=?2",
            params![context.project.to_string(), id.as_str(), applied_at],
        )?;
        applied = true;
    }
    Ok(applied)
}

fn settle(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    plan: &Plan,
    state: &sluice_model::gates::StateSnapshot,
    step: &Step,
    preset: &Preset,
) -> Result<ResultId> {
    let project = context.project.to_string();
    if let Err(error) = check_outputs(step, &preset.outputs) {
        let errors = match error {
            crate::StoreError::Public(PublicError::Invalid { errors, .. }) => errors,
            other => vec![other.to_string()],
        };
        let error = PublicError::Invalid {
            message: "bypass: its outputs no longer match the step's outputs".into(),
            errors,
        };
        tx.sql().execute(
            "UPDATE steps SET status='failed',error=?3,skipped=NULL,manual=0,result_id=NULL WHERE project_id=?1 AND step_id=?2",
            params![project, step.id.as_str(), serde_json::to_string(&error)?],
        )?;
        let result = snapshot_result(tx, context.project, &step.id, None, None)?;
        status_record(
            tx,
            context.project,
            &step.id,
            StepStatus::Pending,
            StepStatus::Failed,
            Some(error),
        )?;
        return Ok(result);
    }
    let hash = inputs_hash(plan, state, step);
    tx.sql().execute("UPDATE steps SET status='succeeded',outputs=?3,inputs_hash=?4,error=NULL,skipped=NULL,manual=1,run_ids='[]',instances='{}',total=NULL,done=0,result_id=NULL WHERE project_id=?1 AND step_id=?2",
        params![project, step.id.as_str(), serde_json::to_string(&preset.outputs)?, hash.map(|h| h.to_string())])?;
    let inputs = effective_map(plan, state, step)?;
    let result = snapshot_result(tx, context.project, &step.id, None, inputs.as_ref())?;
    tx.append_record(
        Some(context.project),
        Event::StepOutput {
            rev: context.revision,
            author: preset.author.clone(),
            reason: preset.reason.clone(),
            step: step.id.clone(),
            outputs: preset.outputs.clone(),
            force: false,
        },
    )?;
    status_record(
        tx,
        context.project,
        &step.id,
        StepStatus::Pending,
        StepStatus::Succeeded,
        None,
    )?;
    Ok(result)
}

/// Whether the step has a preset waiting: the runner never launches it.
pub(crate) fn waiting(
    tx: &WriteTransaction<'_>,
    project: ProjectId,
    step: &StepId,
) -> Result<bool> {
    Ok(preset(tx.sql(), project, step)?.is_some_and(|p| p.pending()))
}
