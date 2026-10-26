//! One latest-run projection shared by status and step_get. No ids are decoded.
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use sluice_agents::activity::{self, Homes, Window};
use sluice_model::{ids::ProjectId, plan_rows::RunTiming, rpc::JsonValue};
use std::path::Path;

pub(crate) struct Run {
    pub id: String,
    pub timing: RunTiming,
    pub agent: Option<sluice_agents::supervisor::AgentObservation>,
    pub time: Option<activity::TimeSplit>,
}
pub(crate) fn home(sql: &Connection) -> &Path {
    sql.path()
        .and_then(|p| Path::new(p).parent())
        .unwrap_or_else(|| Path::new(""))
}
pub(crate) fn run(
    sql: &Connection,
    project: ProjectId,
    step: &str,
    include_time: bool,
) -> sluice_store::Result<Option<Run>> {
    let row = sql.query_row(
        "SELECT r.run_id,coalesce(r.started_at,r.created_at),r.finished_at,max(0,(julianday(coalesce(r.finished_at,'now'))-julianday(coalesce(r.started_at,r.created_at)))*86400),a.request,cast(unixepoch(coalesce(r.started_at,r.created_at),'subsec')*1000 AS INTEGER),cast(unixepoch(r.finished_at,'subsec')*1000 AS INTEGER),r.last_activity_at FROM runs r JOIN attempts a USING(attempt_id) WHERE r.project_id=?1 AND r.step_id=?2 ORDER BY r.created_at DESC,r.run_id DESC LIMIT 1",
        (project.to_string(),step), |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,f64>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?.max(0) as u64,r.get::<_,Option<i64>>(6)?.map(|v| v.max(0) as u64),r.get::<_,Option<String>>(7)?)))
        .optional()?;
    let Some((id, started_at, ended_at, elapsed_s, frozen, start, end, stored_activity)) = row
    else {
        return Ok(None);
    };
    let homes = Homes {
        sluice: home(sql).into(),
        claude: Homes::claude_from_env(),
    };
    let window = Window {
        since_ms: start,
        until_ms: end,
    };
    let mut agent =
        crate::run_health::native(home(sql), &id, &serde_json::from_str::<Value>(&frozen)?);
    if let Some(agent) = &mut agent {
        crate::run_health::observe(&homes, &id, window, agent);
    }
    let last_activity_at = agent
        .as_ref()
        .and_then(|a| a.last_activity_ms)
        .map(activity::rfc3339)
        .or(stored_activity);
    let time = if include_time {
        activity::time_split(
            &homes,
            &id,
            window,
            end.unwrap_or_else(|| crate::run_health::now_seconds().max(0) as u64 * 1000),
        )
    } else {
        None
    };
    let timing = RunTiming {
        started_at,
        ended_at,
        elapsed_s,
        last_activity_at,
        time: time
            .as_ref()
            .map(|t| JsonValue::try_from(json!(t)))
            .transpose()?,
    };
    Ok(Some(Run {
        id,
        timing,
        agent,
        time,
    }))
}
pub(crate) fn latest(
    sql: &Connection,
    project: ProjectId,
    step: &str,
) -> sluice_store::Result<Option<RunTiming>> {
    Ok(run(sql, project, step, false)?.map(|r| r.timing))
}
pub(crate) fn add(row: &mut Value, timing: Option<RunTiming>) -> sluice_store::Result<()> {
    if let Some(timing) = timing {
        for (key, value) in serde_json::to_value(timing)?
            .as_object()
            .into_iter()
            .flatten()
        {
            if key != "time" {
                row[key] = value.clone();
            }
        }
    }
    Ok(())
}
