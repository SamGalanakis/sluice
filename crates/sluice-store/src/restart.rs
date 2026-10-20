//! Durable restart budgets. Decisions use the execution host's clock, so tests advance time
//! without sleeping and a coordinator restart cannot reset a step's rolling-hour budget.
use crate::{
    Result, WriteTransaction,
    plans::{self, PlanContext, RetryMessages},
};
use rusqlite::{Connection, params};
use serde::Serialize;
use sluice_model::{commands::StepStatus, error::PublicError, ids::StepId, plan::Restart};

pub const MAX_PER_HOUR: usize = 6;

#[derive(Debug, Serialize)]
pub struct RestartStatus {
    pub policy: Restart,
    pub count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_at: Option<i64>,
}

// Releases predating restart policies rendered hard-cap resets in the error's head but
// discarded the epoch. The stamp has minute precision: wait through that whole minute.
fn legacy_reset(error: &PublicError) -> Option<i64> {
    let PublicError::AgentFailure { kind, message, .. } = error else {
        return None;
    };
    if kind != "QuotaExhausted" {
        return None;
    }
    let head = message.split(" — ").next()?;
    let stamp = head.split_once("; resets ")?.1.split_whitespace().next()?;
    let minute = stamp.strip_suffix('Z')?;
    time::OffsetDateTime::parse(
        &format!("{minute}:00Z"),
        &time::format_description::well_known::Rfc3339,
    )
    .ok()
    .map(|at| at.unix_timestamp().saturating_add(62))
}

pub fn status(
    sql: &Connection,
    context: &PlanContext,
    step: &StepId,
    now: i64,
) -> Result<Option<RestartStatus>> {
    let spec = &context.plan.steps()[step];
    let Some(policy) = spec.restart else {
        return Ok(None);
    };
    let (count, history, state, error, manual, instances): (i64, String, String, Option<String>, bool, String) = sql.query_row(
        "SELECT restart_count,restart_history,status,error,manual,instances FROM steps WHERE project_id=?1 AND step_id=?2",
        params![context.project.to_string(), step.as_str()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    )?;
    let mut next_at = None;
    if !manual && matches!(state.as_str(), "failed" | "succeeded") {
        let error = error
            .as_deref()
            .map(serde_json::from_str::<PublicError>)
            .transpose()?;
        // A scatter retry retries every failed item. One output failure or cancellation
        // must therefore suppress an engine-only restart, regardless of item order.
        let mut errors: Vec<PublicError> = if spec.scatter.is_some() {
            let instances: serde_json::Value = serde_json::from_str(&instances)?;
            instances
                .as_object()
                .into_iter()
                .flat_map(|m| m.values())
                .filter(|v| v["status"] == "failed")
                .map(|v| serde_json::from_value(v["error"].clone()))
                .collect::<std::result::Result<_, _>>()?
        } else {
            vec![]
        };
        errors.extend(error);
        let cancelled = errors.iter().any(|e| {
            matches!(e, PublicError::Cancelled { .. })
                || matches!(e, PublicError::AgentFailure { kind, .. } if kind == "Cancelled")
        });
        let engine_error = !errors.is_empty()
            && errors.iter().all(|e| match e {
                PublicError::ProcessLost { .. } | PublicError::Transient { .. } => true,
                PublicError::AgentFailure { kind, .. } => matches!(
                    kind.as_str(),
                    "QuotaExhausted"
                        | "WallCap"
                        | "StallCap"
                        | "ReadyTimeout"
                        | "TurnStartTimeout"
                        | "UnknownAcceptance"
                        | "EngineExited"
                        | "Transient"
                        | "Cleanup"
                ),
                _ => false,
            });
        if !cancelled && (policy == Restart::Always || engine_error) {
            // No run means there is no session/work to restart (manual/inline results).
            let finished: Option<String> = sql.query_row(
                "SELECT max(finished_at) FROM runs WHERE project_id=?1 AND step_id=?2 AND generation=(SELECT generation FROM steps WHERE project_id=?1 AND step_id=?2)",
                params![context.project.to_string(), step.as_str()], |r| r.get(0),
            )?;
            let live: bool = sql.query_row(
                "SELECT EXISTS(SELECT 1 FROM attempts WHERE project_id=?1 AND step_id=?2 AND phase<>'terminal')",
                params![context.project.to_string(),step.as_str()], |r|r.get(0),
            )?;
            if let Some(finished) = finished.filter(|_| !live) {
                let finished = time::OffsetDateTime::parse(
                    &finished,
                    &time::format_description::well_known::Rfc3339,
                )
                .map_err(|e| plans::invalid(e.to_string()))?
                .unix_timestamp();
                let history: Vec<i64> = serde_json::from_str(&history)?;
                let recent: Vec<_> = history.into_iter().filter(|at| *at > now - 3600).collect();
                let backoff = (10_i64 << recent.len().min(5)).min(300);
                let mut due = finished
                    .max(recent.last().copied().unwrap_or(finished))
                    .saturating_add(backoff);
                if recent.len() >= MAX_PER_HOUR {
                    due = due.max(recent[recent.len() - MAX_PER_HOUR].saturating_add(3600));
                }
                let reset: Option<i64> = sql.query_row(
                    "SELECT max(restart_after) FROM runs WHERE project_id=?1 AND step_id=?2 AND generation=(SELECT generation FROM steps WHERE project_id=?1 AND step_id=?2) AND work_generation=(SELECT work_generation FROM steps WHERE project_id=?1 AND step_id=?2)",
                    params![context.project.to_string(),step.as_str()], |r|r.get(0),
                )?;
                if let Some(reset) = reset {
                    due = due.max(reset);
                } else if let Some(reset) = errors.iter().filter_map(legacy_reset).max() {
                    due = due.max(reset);
                }
                next_at = Some(due);
            }
        }
    }
    Ok(Some(RestartStatus {
        policy,
        count,
        next_at,
    }))
}

pub fn reconcile(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    now: i64,
    messages: &mut impl RetryMessages,
) -> Result<()> {
    if !context
        .plan
        .steps()
        .values()
        .any(|step| step.restart.is_some())
    {
        return Ok(());
    }
    let state = plans::read_state(tx.sql(), context.project)?;
    if state.paused.is_paused() {
        return Ok(());
    }
    for (id, step) in context.plan.steps() {
        if step.paused.is_paused()
            || step.is_external()
            || !matches!(state.status(id), StepStatus::Failed | StepStatus::Succeeded)
        {
            continue;
        }
        let Some(restart) = status(tx.sql(), context, id, now)? else {
            continue;
        };
        if restart.next_at.is_none_or(|at| at > now) {
            continue;
        }
        let error: Option<String> = tx.sql().query_row(
            "SELECT error FROM steps WHERE project_id=?1 AND step_id=?2",
            params![context.project.to_string(), id.as_str()],
            |r| r.get(0),
        )?;
        let why = error
            .as_deref()
            .map(serde_json::from_str::<PublicError>)
            .transpose()?
            .map(|e| e.to_string())
            .unwrap_or_else(|| "the previous run succeeded".into());
        let body = format!(
            "Automatic restart #{} ({:?}): {}. Resume the same session and continue the step's work.",
            restart.count + 1,
            restart.policy,
            why.chars().take(8000).collect::<String>()
        );
        let mut walk = plans::prepare_retry(
            tx,
            context,
            std::slice::from_ref(id),
            Some(&body),
            "sluice",
            messages,
        )?;
        // Manual retries may re-arm failed dependents. A policy belongs to this step only:
        // it must not revive a cancelled dependent or retry somebody else's output failure.
        walk.rearmed.clear();
        plans::apply_retry(tx, context, walk, Some(&body), "sluice", &body, messages)?;
        let history: String = tx.sql().query_row(
            "SELECT restart_history FROM steps WHERE project_id=?1 AND step_id=?2",
            params![context.project.to_string(), id.as_str()],
            |r| r.get(0),
        )?;
        let mut history: Vec<i64> = serde_json::from_str(&history)?;
        history.retain(|at| *at > now - 3600);
        history.push(now);
        tx.sql().execute("UPDATE steps SET restart_count=restart_count+1,restart_history=?3 WHERE project_id=?1 AND step_id=?2",params![context.project.to_string(),id.as_str(),serde_json::to_string(&history)?])?;
    }
    Ok(())
}
