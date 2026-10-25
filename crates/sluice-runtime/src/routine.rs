//! Operational chatter omitted by next, shared with stored subscriptions.
use rusqlite::Connection;
use sluice_model::{
    commands::{MessageVerb, StepStatus},
    events::{Event, Record},
};

pub(crate) fn routine(
    sql: &Connection,
    record: &Record,
    me: Option<&str>,
) -> sluice_store::Result<bool> {
    Ok(match &record.event {
        Event::StepCancel { author, .. } => me == Some(author.as_str()),
        Event::StepQueued { .. } => true,
        Event::StepStatus {
            step,
            to: StepStatus::Failed,
            error: Some(error),
            run_ids,
            ..
        } if handback(&error.to_string()) => {
            // Text alone cannot hide a failure: require an applied retry action.
            let mut applied = false;
            for run in run_ids {
                applied |= sql.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1 AND step_id=?2 AND json_extract(result,'$.action.outcome')='applied')", (run.to_string(),step.as_str()), |r| r.get::<_,bool>(0))?;
            }
            applied
        }
        Event::Message(m) if m.verb == MessageVerb::Say => {
            let data = m.data.as_ref().map(|v| v.as_value());
            let bypass = m.from == "sluice"
                && me.is_some_and(|me| data.is_some_and(|d| d["step_bypass"]["by"] == me));
            let limit = data.and_then(|d| d.get("execution_limit"));
            let hold = limit.is_some_and(|l| {
                l["outcome"] == "held"
                    || l["outcome"] == "throttled"
                    || (l["limit"] == "memory" && l["outcome"] != "killed")
            });
            let probe = data.is_some_and(|d| d["routine"] == "engine_probe")
                || ((m.from == "sluice" || m.run.is_some())
                    && m.body.starts_with("engine probe observation unavailable"));
            let sendback = if handback(&m.body) {
                sql.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1 AND step_id=?2 AND json_extract(completion_action,'$.target.step')=?3 AND json_extract(completion_action,'$.message')=?4 AND json_extract(result,'$.action.outcome')='applied')", (record.project.map(|p|p.to_string()), &m.from, &m.to, &m.body), |r| r.get(0))?
            } else {
                false
            };
            let legacy_wait = m.from == "sluice"
                && m.body.starts_with("Step ")
                && ((m.body.contains(" has waited since ")
                    && m.body.contains(" execution capacity"))
                    || [
                        "host memory pressure",
                        "memory pressure",
                        "workload memory",
                        "cpu pressure",
                        "cpu load",
                        "cpu warm-up",
                    ]
                    .iter()
                    .any(|reason| m.body.contains(&format!(" is held: {reason}"))));
            bypass || (m.from == "sluice" && hold) || legacy_wait || probe || sendback
        }
        _ => false,
    })
}
fn handback(text: &str) -> bool {
    text.starts_with("rebase onto origin/main conflicts in ")
        || text.starts_with(
            "not ready: the work step submitted ready=false. Unresolved: awaiting slice",
        )
}
