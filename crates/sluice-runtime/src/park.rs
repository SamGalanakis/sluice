//! Leases and parking from inside a run: `lease_acquire`, `lease_release`, `lease_status`
//! and `park`, and the run supervisor's side of a park (`CoordinatorCommand::Park`).
use crate::{calls::public, coordinator::Coordinator, execution::ExecutionHost};
use serde_json::{Value, json};
use sluice_model::{
    commands::*,
    error::PublicError,
    ids::*,
    park::{ParkCondition, ParkOp, ParkView},
};
use sluice_process::journal::AttemptKey;
use sluice_store::{
    RetrySafety, messages,
    resources::{self, ParkAsked},
    writer::ChangeKey,
};
use std::time::Duration;

/// The longest `lease_acquire --wait` holds its request, and its default.
pub const MAX_LEASE_WAIT_SECONDS: u64 = 600;
/// The engines whose sessions a park holds idle and resumes.
const PARKING_ENGINES: [&str; 3] = ["claude", "codex", "fake"];

fn bad(message: impl Into<String>) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}
fn data(value: Value) -> Result<CommandReply, PublicError> {
    Ok(CommandReply::Data(value.try_into()?))
}

/// The run must be one of the project's.
fn project_run(
    sql: &rusqlite::Connection,
    project: &ProjectSelector,
    run: RunId,
) -> sluice_store::Result<ProjectId> {
    let project = messages::resolve_project(sql, project)?;
    let owner: Option<Option<String>> = rusqlite::OptionalExtension::optional(sql.query_row(
        "SELECT project_id FROM runs WHERE run_id=?1",
        [run.to_string()],
        |r| r.get(0),
    ))?;
    if owner.flatten() != Some(project.to_string()) {
        return Err(PublicError::BadRequest {
            message: format!("run {run} is not a run of this project"),
        }
        .into());
    }
    Ok(project)
}

fn lease_view(all: &[resources::Lease], lease: LeaseId) -> Value {
    match all.iter().find(|l| l.id == lease) {
        Some(l) => json!({
            "lease": l.id,
            "resource": l.resource,
            "amount": l.amount,
            "priority": l.priority,
            "state": if l.state == LeaseState::Held { "held" } else { "waiting" },
            "position": resources::queue_position(all, lease),
        }),
        None => json!({"lease": lease, "state": "released"}),
    }
}

fn run_project(sql: &rusqlite::Connection, run: RunId) -> sluice_store::Result<ProjectId> {
    let project: String = sql.query_row(
        "SELECT project_id FROM runs WHERE run_id=?1",
        [run.to_string()],
        |r| r.get(0),
    )?;
    project
        .parse()
        .map_err(|_| sluice_store::StoreError::InvalidDatabase("run project id".into()))
}

/// The four run tools; `None` for any other command.
pub async fn dispatch<H: ExecutionHost>(
    broker: &Coordinator<H>,
    request: &CommandRequest,
) -> Result<Option<CommandReply>, PublicError> {
    Ok(Some(match request.clone() {
        CommandRequest::LeaseAcquire(request) => acquire(broker, request).await?,
        CommandRequest::LeaseRelease(request) => broker
            .writer()
            .write(RetrySafety::Idempotent, move |tx| {
                let project = project_run(tx.sql(), &request.project, request.run)?;
                let released = resources::release_lease(tx, request.lease, request.run)?;
                if !released {
                    return Err(PublicError::BadRequest {
                        message: format!(
                            "lease {} is not a waiting or held lease of run {}",
                            request.lease, request.run
                        ),
                    }
                    .into());
                }
                // The next in line gets it now, not at the next scheduler pass.
                resources::grant_leases(tx, project)?;
                Ok(json!({"lease": request.lease, "state": "released"}))
            })
            .await
            .and_then(data)?,
        CommandRequest::LeaseStatus(request) => broker
            .reads()
            .snapshot(move |sql| {
                let project = messages::resolve_project(sql, &request.project)?;
                resources::lease_status(sql, project, request.resource.as_deref())
            })
            .await
            .map_err(public)
            .and_then(data)?,
        CommandRequest::Park(request) => park(broker, request).await?,
        _ => return Ok(None),
    }))
}

async fn acquire<H: ExecutionHost>(
    broker: &Coordinator<H>,
    request: LeaseAcquire,
) -> Result<CommandReply, PublicError> {
    let amount = request.amount.unwrap_or(1);
    if amount == 0 {
        return Err(bad("amount must be a positive integer"));
    }
    let timeout = request
        .timeout_seconds
        .unwrap_or(MAX_LEASE_WAIT_SECONDS)
        .min(MAX_LEASE_WAIT_SECONDS);
    let wait = request.wait;
    let (project, lease) = broker
        .writer()
        .write(RetrySafety::NonIdempotent, move |tx| {
            let project = project_run(tx.sql(), &request.project, request.run)?;
            let lease = resources::request_lease_ranked(
                tx,
                request.run,
                &request.resource,
                amount,
                &format!("cli/{}/{}", request.run, RunId::new()),
                request.priority,
            )?;
            resources::grant_leases(tx, project)?;
            Ok((project, lease))
        })
        .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
    loop {
        // Subscribe before reading, so a grant between the two still wakes the wait.
        let mut changes = broker
            .reads()
            .subscribe(
                broker.writer(),
                vec![ChangeKey::new(Some(project), "resources")],
            )
            .await
            .map_err(public)?;
        let view = broker
            .reads()
            .snapshot(move |sql| Ok(lease_view(&resources::leases(sql, project)?, lease)))
            .await
            .map_err(public)?;
        let waiting = view["state"] == "waiting";
        if !waiting || !wait {
            return data(view);
        }
        if tokio::time::Instant::now() >= deadline {
            let mut view = view;
            view["timed_out"] = json!(true);
            return data(view);
        }
        tokio::select! {
            result = changes.wait() => { result.map_err(public)?; }
            _ = tokio::time::sleep_until(deadline) => {}
        }
    }
}

/// The step's fn and engine, from its frozen request: `(fn, engine input)`.
fn run_engine(
    sql: &rusqlite::Connection,
    run: RunId,
) -> sluice_store::Result<(Option<String>, Option<String>)> {
    Ok(sql.query_row(
        "SELECT json_extract(a.request,'$.declaration.run'),json_extract(a.request,'$.inputs.engine') FROM runs r JOIN attempts a USING(attempt_id) WHERE r.run_id=?1",
        [run.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

async fn park<H: ExecutionHost>(
    broker: &Coordinator<H>,
    request: Park,
) -> Result<CommandReply, PublicError> {
    let mut condition = ParkCondition::parse(&request.until)?;
    if let ParkCondition::Pid { start, .. } = &mut condition {
        *start = request.pid_start;
    }
    let run = request.run;
    let selector = request.project.clone();
    let (function, engine) = broker
        .reads()
        .snapshot(move |sql| {
            project_run(sql, &selector, run)?;
            run_engine(sql, run)
        })
        .await
        .map_err(public)?;
    let engine = engine.or_else(|| {
        function
            .as_deref()
            .and_then(|f| f.strip_prefix("agent."))
            .map(str::to_owned)
    });
    match engine.as_deref() {
        Some(engine) if PARKING_ENGINES.contains(&engine) => {}
        Some("devin") => {
            return Err(bad(
                "park is not available on the devin engine: its session cannot be held idle and resumed; wait with `sluice tool lease acquire --wait` instead",
            ));
        }
        _ => {
            return Err(bad(
                "park needs an agent run on the claude or codex engine; a fn waits for a lease with ctx.acquire",
            ));
        }
    }
    let timeout = request
        .timeout_seconds
        .unwrap_or_else(|| crate::config::HomeConfig::load(broker.home()).park_timeout_seconds);
    if timeout == 0 {
        return Err(bad("timeout must be a positive number of seconds"));
    }
    if sluice_agents::supervisor::condition_holds(&condition).map_err(|e| bad(e.to_string()))? {
        return data(json!({
            "parked": false,
            "until": condition.to_string(),
            "message": format!("{condition} holds already: carry on, nothing was parked."),
        }));
    }
    let until = condition.to_string();
    let asked = broker
        .writer()
        .write(RetrySafety::NonIdempotent, move |tx| {
            resources::park(tx, run, condition, Some(timeout))
        })
        .await?;
    match asked {
        ParkAsked::Ready { lease } => data(json!({
            "parked": false,
            "until": until,
            "lease": lease,
            "message": format!("{until} is granted already (lease {}): carry on, nothing was parked; release it with `sluice tool lease release` when done.", lease.map(|l| l.to_string()).unwrap_or_default()),
        })),
        ParkAsked::Parked(park) => {
            let position = match park.lease {
                Some(lease) => broker
                    .reads()
                    .snapshot(move |sql| {
                        Ok(resources::queue_position(
                            &resources::leases(sql, run_project(sql, run)?)?,
                            lease,
                        ))
                    })
                    .await
                    .map_err(public)?,
                None => None,
            };
            data(json!({
                "parked": true,
                "until": until,
                "lease": park.lease,
                "position": position,
                "timeout_seconds": park.timeout_seconds,
                "message": format!("Parked until {until}, a message, or {timeout}s pass. End your turn now and do nothing else: your step's lane is released while you wait, and this session resumes with a message that starts \"woke:\" when it fires."),
            }))
        }
    }
}

/// The run supervisor's side of its park.
pub async fn supervise<H: ExecutionHost>(
    broker: &Coordinator<H>,
    id: AttemptKey,
    op: ParkOp,
) -> Result<ParkView, PublicError> {
    let run = id.run;
    let attempt = id.attempt;
    let view = move |sql: &rusqlite::Connection,
                     park: Option<sluice_model::park::ParkRequest>,
                     woke: bool|
          -> sluice_store::Result<ParkView> {
        let cancelled: bool = sql.query_row(
            "SELECT cancel_requested FROM attempts WHERE attempt_id=?1",
            [attempt.to_string()],
            |r| r.get(0),
        )?;
        let leases = resources::run_leases(sql, run)?;
        let lease_held = park.as_ref().and_then(|p| p.lease).is_some_and(|lease| {
            leases
                .iter()
                .any(|l| l.id == lease && l.state == LeaseState::Held)
        });
        Ok(ParkView {
            park,
            lease_held,
            leases_active: !leases.is_empty(),
            woke,
            cancelled,
        })
    };
    match op {
        ParkOp::Read => broker
            .reads()
            .snapshot(move |sql| view(sql, resources::park_request(sql, run)?, false))
            .await
            .map_err(public),
        ParkOp::Enter => {
            broker
                .writer()
                .write(RetrySafety::Idempotent, move |tx| {
                    let park = resources::park_enter(tx, run)?;
                    view(tx.sql(), park, false)
                })
                .await
        }
        ParkOp::Wake { reason } => {
            broker
                .writer()
                .write(RetrySafety::Idempotent, move |tx| {
                    let (woke, park) = resources::park_wake(tx, run, &reason)?;
                    view(tx.sql(), park, woke)
                })
                .await
        }
    }
}
