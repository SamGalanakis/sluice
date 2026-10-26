//! Agent checks and outcome counts. Extra run columns keep pinned releases' wire intact.
use crate::{coordinator::Coordinator, execution::ExecutionHost};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use sluice_agents::{model, supervisor::AgentObservation};
use sluice_model::{
    error::PublicError,
    ids::{ProjectId, RunId},
    rpc::JsonValue,
};
use sluice_store::{RetrySafety, WriteTransaction, identities, messages};
use std::{collections::BTreeMap, path::Path};

pub fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn home(sql: &Connection) -> &Path {
    sql.path()
        .and_then(|p| Path::new(p).parent())
        .unwrap_or_else(|| Path::new(""))
}
pub(crate) fn native(home: &Path, run: &str, frozen: &Value) -> Option<AgentObservation> {
    let dir = home.join("runs").join(run);
    let observed = std::fs::read(dir.join("agent-observation.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<AgentObservation>(&b).ok());
    if observed.is_some() {
        return observed;
    }
    // Guardians pinned before observation snapshots keep their engine and last words here.
    let checkpoint = std::fs::read(dir.join("native.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(Value::Null);
    let name = frozen["declaration"]["run"]
        .as_str()
        .or(frozen["function"]["name"].as_str())
        .unwrap_or_default();
    let engine = checkpoint["engine"]
        .as_str()
        .or(frozen["inputs"]["engine"].as_str())
        .or(match name {
            "agent.codex" => Some("codex"),
            "agent.claude" | "agent.review" | "decide.llm" => Some("claude"),
            "agent.devin" => Some("devin"),
            _ => None,
        })?;
    let model = match frozen["inputs"].get("model").filter(|m| !m.is_null()) {
        Some(value) => serde_json::from_value::<model::ModelChoice>(value.clone())
            .ok()
            .and_then(|m| model::compose(engine, &m).ok())
            .map(|m| m.id)
            .or_else(|| {
                Some(
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                )
            }),
        None => model::default_for(engine)
            .and_then(|m| model::compose(engine, &m).ok())
            .map(|m| m.id),
    };
    Some(AgentObservation {
        engine: engine.into(),
        model,
        last_activity_ms: None,
        last_message: checkpoint["final_text"].as_str().unwrap_or_default().into(),
    })
}
/// Called on completion so counts remain available after pruning and artifact retention.
pub(crate) fn record_agent(tx: &mut WriteTransaction<'_>, run: RunId) -> sluice_store::Result<()> {
    let raw: String = tx.sql().query_row(
        "SELECT request FROM attempts JOIN runs USING(attempt_id) WHERE run_id=?1",
        [run.to_string()],
        |r| r.get(0),
    )?;
    if let Some(mut agent) = native(
        home(tx.sql()),
        &run.to_string(),
        &serde_json::from_str(&raw)?,
    ) {
        let (start,end) = tx.sql().query_row("SELECT cast(unixepoch(coalesce(started_at,created_at),'subsec')*1000 AS INTEGER),cast(unixepoch(finished_at,'subsec')*1000 AS INTEGER) FROM runs WHERE run_id=?1", [run.to_string()], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,Option<i64>>(1)?)))?;
        observe(
            &sluice_agents::activity::Homes {
                sluice: home(tx.sql()).into(),
                claude: sluice_agents::activity::Homes::claude_from_env(),
            },
            &run.to_string(),
            sluice_agents::activity::Window {
                since_ms: start.max(0) as u64,
                until_ms: end.map(|t| t.max(0) as u64),
            },
            &mut agent,
        );
        tx.sql().execute(
            "UPDATE runs SET agent_engine=?2,agent_model=?3,last_activity_at=?4 WHERE run_id=?1",
            (
                run.to_string(),
                agent.engine,
                agent.model,
                agent.last_activity_ms.map(sluice_agents::activity::rfc3339),
            ),
        )?;
    }
    Ok(())
}

pub(crate) fn report(
    sql: &Connection,
    project: ProjectId,
    since: Option<String>,
) -> sluice_store::Result<Value> {
    let after: Option<i64> = match since.as_deref() {
        Some(s) => Some(
            sql.query_row("SELECT unixepoch(?1)", [s], |r| r.get::<_, Option<i64>>(0))?
                .ok_or_else(|| PublicError::BadRequest {
                    message: "since must be an RFC 3339 timestamp".into(),
                })?,
        ),
        None => None,
    };
    let mut q = sql.prepare("SELECT r.run_id,r.agent_engine,r.agent_model,r.result,r.failure_class,a.request FROM runs r JOIN attempts a USING(attempt_id) WHERE r.project_id=?1 AND (?2 IS NULL OR unixepoch(coalesce(r.started_at,r.created_at))>=?2) ORDER BY r.created_at,r.run_id")?;
    let mut rows = q.query((project.to_string(), after))?;
    let mut engines = BTreeMap::<(String, String), Value>::new();
    while let Some(row) = rows.next()? {
        let run: String = row.get(0)?;
        let engine: Option<String> = row.get(1)?;
        let model: Option<String> = row.get(2)?;
        let result: Option<String> = row.get(3)?;
        let class: Option<String> = row.get(4)?;
        let raw: String = row.get(5)?;
        let agent = if let Some(engine) = engine {
            Some(AgentObservation {
                engine,
                model,
                ..Default::default()
            })
        } else {
            native(home(sql), &run, &serde_json::from_str(&raw)?)
        };
        let Some(agent) = agent else { continue };
        let model = agent.model.unwrap_or_else(|| "unknown".into());
        let entry = engines.entry((agent.engine.clone(), model.clone())).or_insert_with(|| json!({"engine":agent.engine,"model":model,"attempts":0,"succeeded":0,"failed":{}}));
        entry["attempts"] = json!(entry["attempts"].as_u64().unwrap_or(0) + 1);
        if let Some(raw) = result {
            let result: Value = serde_json::from_str(&raw)?;
            if result["status"] == "succeeded" {
                entry["succeeded"] = json!(entry["succeeded"].as_u64().unwrap_or(0) + 1);
            } else {
                let class = class
                    .or_else(|| {
                        serde_json::from_value::<PublicError>(result["error"].clone())
                            .ok()
                            .and_then(|e| serde_json::to_value(e.failure_class()).ok())
                            .and_then(|v| v.as_str().map(str::to_owned))
                    })
                    .unwrap_or_else(|| "harness".into());
                entry["failed"][&class] = json!(entry["failed"][&class].as_u64().unwrap_or(0) + 1);
            }
        }
    }
    Ok(
        json!({"project_id":project,"since":since,"engines":engines.into_values().collect::<Vec<_>>()}),
    )
}

/// Explicit wall-clock seam. Production's scheduler supplies system time; tests advance it.
pub async fn check_at<H: ExecutionHost>(
    broker: &Coordinator<H>,
    now: i64,
) -> Result<(), PublicError> {
    refresh_activity(broker).await?;
    let root = broker.home().to_owned();
    let candidates = broker
        .reads()
        .snapshot(move |sql| candidates(sql, &root, now))
        .await
        .map_err(|e| e.into_public(true))?;
    if candidates.is_empty() {
        return Ok(());
    }
    broker.writer().write(RetrySafety::Idempotent, move |tx| {
        for candidate in candidates {
            let Some(age) = tx.sql().query_row("SELECT p.check_age_seconds FROM runs r JOIN projects p USING(project_id) WHERE r.run_id=?1 AND r.finished_at IS NULL AND p.deleted_at IS NULL", [&candidate.run], |r| r.get::<_,i64>(0)).optional()? else { continue };
            if age == 0 || age != candidate.age { continue; }
            let changed = tx.sql().execute("UPDATE runs SET check_notified=?2,agent_engine=?3,agent_model=?4 WHERE run_id=?1 AND finished_at IS NULL AND check_notified<?2", (&candidate.run,candidate.threshold,&candidate.agent.engine,&candidate.agent.model))?;
            if changed == 0 { continue; }
            let owner = identities::route_run(tx.sql(),candidate.project,Some(&candidate.step),None)?;
            let data = json!({"run_check": {
                "unit":candidate.unit,"step":candidate.step,"run":candidate.run,
                "engine":candidate.agent.engine,"model":candidate.agent.model,
                "elapsed_seconds":candidate.elapsed,"check_age_seconds":age,
                "last_activity_at":candidate.agent.last_activity_ms.map(sluice_agents::activity::rfc3339),
                "last_activity_ago_seconds":candidate.agent.last_activity_ms.map(|ms| now.saturating_sub((ms/1000) as i64).max(0)),
                "last_message":candidate.agent.last_message,"parked":candidate.parked,"waiting":candidate.waiting
            }});
            let activity = data["run_check"]["last_activity_at"].as_str().map(|at| format!("{at}, {}s ago",data["run_check"]["last_activity_ago_seconds"])).unwrap_or_else(|| "unknown".into());
            let body = format!("Check unit {} step {}: {} · {}. Elapsed {}s. Last observed agent activity: {}. Parked: {}. Waiting on resources: {}.{}", candidate.unit,candidate.step,candidate.agent.engine,candidate.agent.model.as_deref().unwrap_or("unknown"),candidate.elapsed,activity,candidate.parked,candidate.waiting.join(", "),if candidate.agent.last_message.is_empty() { String::new() } else { format!("\nLast worker message: {}",candidate.agent.last_message) });
            messages::post(tx,messages::Post { project: sluice_model::ids::ProjectSelector::Id(candidate.project), speaker: messages::Speaker::Sluice,body,verb: messages::Verb::Say {to:owner,data:Some(JsonValue::try_from(data)?) } }, &messages::NoPlanInputs)?;
        }
        Ok(())
    }).await
}
struct Candidate {
    project: ProjectId,
    run: String,
    step: String,
    unit: String,
    agent: AgentObservation,
    age: i64,
    threshold: i64,
    elapsed: i64,
    parked: bool,
    waiting: Vec<String>,
}
fn candidates(sql: &Connection, root: &Path, now: i64) -> sluice_store::Result<Vec<Candidate>> {
    let mut q = sql.prepare("SELECT r.project_id,r.run_id,r.step_id,coalesce(r.unit,r.step_id),p.check_age_seconds,?1-unixepoch(coalesce(r.started_at,r.created_at)),a.request FROM runs r JOIN attempts a USING(attempt_id) JOIN projects p USING(project_id) WHERE r.finished_at IS NULL AND r.step_id IS NOT NULL AND p.deleted_at IS NULL AND p.check_age_seconds>0 AND ?1-unixepoch(coalesce(r.started_at,r.created_at))>=p.check_age_seconds AND (?1-unixepoch(coalesce(r.started_at,r.created_at)))/p.check_age_seconds*p.check_age_seconds>r.check_notified")?;
    let mut rows = q.query([now])?;
    let mut out = vec![];
    while let Some(row) = rows.next()? {
        let run: String = row.get(1)?;
        let raw: String = row.get(6)?;
        let frozen: Value = serde_json::from_str(&raw)?;
        let Some(mut agent) = native(root, &run, &frozen) else {
            continue;
        };
        let elapsed: i64 = row.get(5)?;
        observe(
            &sluice_agents::activity::Homes {
                sluice: root.to_owned(),
                claude: sluice_agents::activity::Homes::claude_from_env(),
            },
            &run,
            sluice_agents::activity::Window {
                since_ms: ((now - elapsed).max(0) as u64) * 1000,
                until_ms: None,
            },
            &mut agent,
        );
        agent.last_message = sluice_agents::engines::account::redact(&agent.last_message)
            .chars()
            .take(4000)
            .collect();
        let waiting = sql
            .prepare(
                "SELECT resource FROM leases WHERE run_id=?1 AND state='waiting' ORDER BY resource",
            )?
            .query_map([&run], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        let age: i64 = row.get(4)?;
        out.push(Candidate {
            project: row
                .get::<_, String>(0)?
                .parse()
                .map_err(|e| sluice_store::StoreError::InvalidDatabase(format!("{e}")))?,
            run,
            step: row.get(2)?,
            unit: row.get(3)?,
            agent,
            age,
            threshold: elapsed / age * age,
            elapsed,
            parked: frozen["park"]["phase"] == "parked",
            waiting,
        });
    }
    Ok(out)
}
/// Next boundary and whether it is overdue, for each current agent run of a step.
pub(crate) fn due(
    sql: &Connection,
    project: ProjectId,
    step: &str,
    now: i64,
) -> sluice_store::Result<Vec<Value>> {
    let mut q = sql.prepare("SELECT r.run_id,unixepoch(coalesce(r.started_at,r.created_at)),r.check_notified,p.check_age_seconds,a.request FROM runs r JOIN attempts a USING(attempt_id) JOIN projects p USING(project_id) WHERE r.project_id=?1 AND r.step_id=?2 AND r.finished_at IS NULL AND p.check_age_seconds>0")?;
    let mut rows = q.query((project.to_string(), step))?;
    let mut out = vec![];
    while let Some(row) = rows.next()? {
        let run: String = row.get(0)?;
        let raw: String = row.get(4)?;
        if native(home(sql), &run, &serde_json::from_str(&raw)?).is_none() {
            continue;
        }
        let start: i64 = row.get(1)?;
        let notified: i64 = row.get(2)?;
        let age: i64 = row.get(3)?;
        let at = start.saturating_add((notified / age + 1).saturating_mul(age));
        out.push(json!({"step":step,"run":run,"at":at,"overdue":now>=at}));
    }
    Ok(out)
}

/// The observation snapshot is authoritative; older guardians use the cached transcript.
pub(crate) fn observe(
    homes: &sluice_agents::activity::Homes,
    run: &str,
    window: sluice_agents::activity::Window,
    agent: &mut AgentObservation,
) {
    if agent.last_activity_ms.is_some() {
        return;
    }
    if let Some(outline) = sluice_agents::activity::outline(homes, run, window) {
        agent.last_activity_ms = outline
            .turns
            .iter()
            .filter_map(|t| t.ended_ms)
            .chain(
                outline
                    .calls()
                    .flat_map(|c| [c.started_ms, c.ended_ms].into_iter().flatten()),
            )
            .max();
        if let Some(turn) = outline.turns.iter().rev().find(|t| !t.said.is_empty()) {
            agent.last_message.clone_from(&turn.said);
        }
    }
}

/// Persist the same observed activity for SQL readers, including runs below the check
/// threshold and projects whose long-running notifications are disabled.
async fn refresh_activity<H: ExecutionHost>(broker: &Coordinator<H>) -> Result<(), PublicError> {
    let root = broker.home().to_owned();
    let observed = broker.reads().snapshot(move |sql| {
        let mut q = sql.prepare("SELECT r.run_id,a.request,r.last_activity_at,cast(unixepoch(coalesce(r.started_at,r.created_at),'subsec')*1000 AS INTEGER) FROM runs r JOIN attempts a USING(attempt_id) WHERE r.finished_at IS NULL")?;
        let mut rows = q.query([])?;
        let mut updates = vec![];
        let homes = sluice_agents::activity::Homes { sluice: root.clone(), claude: sluice_agents::activity::Homes::claude_from_env() };
        while let Some(row) = rows.next()? {
            let run: String = row.get(0)?;
            let frozen: String = row.get(1)?;
            let old: Option<String> = row.get(2)?;
            if let Some(mut agent) = native(&root, &run, &serde_json::from_str(&frozen)?) {
                observe(&homes, &run, sluice_agents::activity::Window { since_ms: row.get::<_,i64>(3)?.max(0) as u64, until_ms: None }, &mut agent);
                let at = agent.last_activity_ms.map(sluice_agents::activity::rfc3339);
                if at.is_some() && at != old { updates.push((run,at)); }
            }
        }
        Ok(updates)
    }).await.map_err(|e| e.into_public(true))?;
    if observed.is_empty() {
        return Ok(());
    }
    broker
        .writer()
        .write(RetrySafety::Idempotent, move |tx| {
            for (run, at) in observed {
                tx.sql().execute(
                    "UPDATE runs SET last_activity_at=?2 WHERE run_id=?1",
                    (run, at),
                )?;
            }
            tx.changed(None, "status");
            Ok(())
        })
        .await
}
