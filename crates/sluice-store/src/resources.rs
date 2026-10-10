//! Resource declarations and one durable accounting for needs and section leases.
//!
//! Compose mutations inside the reservation/terminalization writer transaction.
//! `runs.finished_at` is the attempts owner's durable assertion of proven stop,
//! including an empty owned payload subtree. Cancellation alone is not that proof.
use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sluice_model::{
    commands::{LeaseState, StepStatus},
    error::PublicError,
    events::Event,
    gates::{GateDecision, StateSnapshot, StepState, readiness},
    ids::{LeaseId, ProjectId, Revision, RunId, StepId},
    plan::{Pause, Plan, SignatureProvider},
    types::Type,
};

use crate::{Result, StoreError, WriteTransaction};

pub type Needs = BTreeMap<String, u64>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capacity {
    Fixed(u64),
    Function(String),
}
#[derive(Debug, Clone, PartialEq)]
pub struct Resource {
    pub name: String,
    pub declaration: Capacity,
    pub capacity: Option<u64>,
    pub revision: i64,
    pub error: Option<PublicError>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Lease {
    pub id: LeaseId,
    pub run: RunId,
    pub step: Option<StepId>,
    pub resource: String,
    pub amount: u64,
    pub priority: i64,
    pub state: LeaseState,
    pub since: String,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceStatus {
    pub resource: Resource,
    pub held: u64,
    pub queued: usize,
    pub holders: Vec<Lease>,
    pub waiting: Vec<Lease>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    pub step: StepId,
    pub needs: Needs,
    pub priority: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fit {
    pub blocked: Vec<String>,
    /// Empty when it fits; otherwise e.g. `needs lane 1 (56/56 held)`.
    pub reason: String,
}
impl Fit {
    pub fn fits(&self) -> bool {
        self.blocked.is_empty()
    }
}

fn bad(message: impl Into<String>) -> StoreError {
    PublicError::BadRequest {
        message: message.into(),
    }
    .into()
}
fn conflict(message: impl Into<String>) -> StoreError {
    PublicError::Conflict {
        message: message.into(),
        current_rev: None,
    }
    .into()
}
fn integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| bad("resource amount exceeds SQLite integer range"))
}
fn now() -> Result<String> {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| StoreError::InvalidDatabase(e.to_string()))
}
fn id<T: std::str::FromStr>(text: String) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    text.parse()
        .map_err(|e: T::Err| StoreError::InvalidDatabase(e.to_string()))
}
fn changed(tx: &mut WriteTransaction<'_>, project: ProjectId) {
    for view in ["resources", "status", "plan"] {
        tx.changed(Some(project), view);
    }
}
fn declaration(raw: &Value) -> Result<Capacity> {
    if let Some(n) = raw.as_u64() {
        integer(n)?;
        return Ok(Capacity::Fixed(n));
    }
    if let Some(map) = raw.as_object()
        && map.len() == 1
    {
        if let Some(n) = map.get("capacity").and_then(Value::as_u64) {
            integer(n)?;
            return Ok(Capacity::Fixed(n));
        }
        if let Some(name) = map.get("capacity_fn").and_then(Value::as_str)
            && !name.is_empty()
        {
            return Ok(Capacity::Function(name.into()));
        }
    }
    Err(bad(
        "expected an integer >= 0, {capacity: integer >= 0}, or {capacity_fn: fn}",
    ))
}
fn canonical(cap: &Capacity) -> Value {
    match cap {
        Capacity::Fixed(n) => json!({"capacity":n}),
        Capacity::Function(name) => json!({"capacity_fn":name}),
    }
}

pub fn declarations(conn: &Connection, project: ProjectId) -> Result<BTreeMap<String, Resource>> {
    let mut stmt = conn.prepare("SELECT name,declaration,coalesce(capacity,observed_capacity),revision,error FROM resources WHERE project_id=? ORDER BY name")?;
    let rows = stmt.query_map([project.to_string()], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?.map(|n| n as u64),
            r.get::<_, i64>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (name, raw, capacity, revision, error) = row?;
        out.insert(
            name.clone(),
            Resource {
                name,
                declaration: declaration(&serde_json::from_str(&raw)?)?,
                capacity,
                revision,
                error: error.map(|e| serde_json::from_str(&e)).transpose()?,
            },
        );
    }
    Ok(out)
}

/// Validates the entire patch before writing. Null removes; unmentioned names stay.
/// The registry provider must reflect the project's visible fn signatures.
pub fn patch_resources(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    patch: &Value,
    signatures: &impl SignatureProvider,
) -> Result<bool> {
    let map = patch
        .as_object()
        .ok_or_else(|| bad("resources: expected an object"))?;
    let existing = declarations(tx.sql(), project)?;
    let revision: i64 = tx
        .sql()
        .query_row(
            "SELECT resources_rev+1 FROM projects WHERE project_id=? AND deleted_at IS NULL",
            [project.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| bad("resource project does not exist"))?;
    let mut edits = Vec::new();
    for (name, value) in map {
        StepId::new(name).map_err(|_| bad(format!("resources.{name}: invalid resource name")))?;
        let next = if value.is_null() {
            None
        } else {
            Some(declaration(value)?)
        };
        if next.as_ref() == existing.get(name).map(|r| &r.declaration) {
            continue;
        }
        if let Some(Capacity::Function(fn_name)) = &next {
            let sig = signatures.signature(fn_name).ok_or_else(|| {
                bad(format!(
                    "resources.{name}: the project sees no fn '{fn_name}'"
                ))
            })?;
            if fn_name == "core.external"
                || !matches!(sig.outputs.get("capacity"), Some(Type::Int | Type::Any))
                || sig.inputs.values().any(|t| !matches!(t, Type::Optional(_)))
            {
                return Err(bad(format!(
                    "resources.{name}: capacity fn must take no required inputs and have an int output capacity"
                )));
            }
        }
        if next.is_none() {
            let referenced: bool = tx.sql().query_row("SELECT EXISTS(SELECT 1 FROM steps s,json_each(s.declaration,'$.needs') n WHERE s.project_id=?1 AND n.key=?2) OR EXISTS(SELECT 1 FROM leases WHERE project_id=?1 AND resource=?2 AND state IN ('waiting','held'))", params![project.to_string(), name], |r| r.get(0))?;
            if referenced {
                return Err(bad(format!(
                    "resources.{name}: a step needs it or holds or waits for a lease"
                )));
            }
        }
        edits.push((name, next));
    }
    if edits.is_empty() {
        return Ok(false);
    }
    for (name, next) in edits {
        if let Some(cap) = next {
            let fixed = match cap {
                Capacity::Fixed(n) => Some(integer(n)?),
                Capacity::Function(_) => None,
            };
            tx.sql().execute("INSERT INTO resources(scope,project_id,name,declaration,capacity,revision) VALUES (?1,?1,?2,?3,?4,?5) ON CONFLICT(scope,name) DO UPDATE SET declaration=excluded.declaration,capacity=excluded.capacity,revision=excluded.revision,observed_capacity=NULL,observed_at=NULL,error=NULL", params![project.to_string(), name, canonical(&cap).to_string(), fixed, revision])?;
        } else {
            // Inactive rows are history, not live references. LeaseId is never reused.
            tx.sql().execute("DELETE FROM leases WHERE project_id=?1 AND resource=?2 AND state IN ('released','cancelled')", params![project.to_string(), name])?;
            tx.sql().execute(
                "DELETE FROM resources WHERE project_id=?1 AND name=?2",
                params![project.to_string(), name],
            )?;
        }
    }
    tx.sql().execute(
        "UPDATE projects SET resources_rev=?2 WHERE project_id=?1",
        params![project.to_string(), revision],
    )?;
    changed(tx, project);
    Ok(true)
}

/// Stale observations (including remove/redeclare) are ignored by revision.
/// Failures retain the last good capacity. External fn work happens outside SQL.
pub fn observe_capacity(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    name: &str,
    revision: i64,
    observation: std::result::Result<u64, PublicError>,
) -> Result<bool> {
    let resources = declarations(tx.sql(), project)?;
    let Some(resource) = resources.get(name) else {
        return Ok(false);
    };
    let Capacity::Function(fn_name) = &resource.declaration else {
        return Ok(false);
    };
    if revision != resource.revision {
        return Ok(false);
    }
    let (capacity, error) = match observation {
        Ok(n) => {
            integer(n)?;
            (Some(n), None)
        }
        Err(e) => (resource.capacity, Some(e)),
    };
    if capacity == resource.capacity && error == resource.error {
        return Ok(false);
    }
    tx.sql().execute("UPDATE resources SET observed_capacity=?3,error=?4,observed_at=?5 WHERE project_id=?1 AND name=?2", params![project.to_string(), name, capacity.map(integer).transpose()?, error.as_ref().map(serde_json::to_string).transpose()?, now()?])?;
    tx.append_record(
        Some(project),
        Event::ProjectCapacity {
            resource: name.into(),
            name: fn_name.clone(),
            capacity,
            error,
        },
    )?;
    changed(tx, project);
    Ok(true)
}

/// During drain, only observations needed by admitted work remain eligible.
pub fn capacity_observations(conn: &Connection, project: ProjectId) -> Result<Vec<Resource>> {
    let mode: String =
        conn.query_row("SELECT mode FROM maintenance WHERE singleton=1", [], |r| {
            r.get(0)
        })?;
    let mut out = vec![];
    for resource in declarations(conn, project)?.into_values() {
        if !matches!(resource.declaration, Capacity::Function(_)) {
            continue;
        }
        let needed: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM leases WHERE project_id=?1 AND resource=?2 AND state IN ('waiting','held'))", params![project.to_string(), resource.name], |r| r.get(0))?;
        if mode == "normal" || needed {
            out.push(resource);
        }
    }
    Ok(out)
}

pub fn held(conn: &Connection, project: ProjectId) -> Result<Needs> {
    let mut stmt =
        conn.prepare("SELECT resource,amount FROM leases WHERE project_id=? AND state='held'")?;
    let rows = stmt.query_map([project.to_string()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
    })?;
    let mut out = Needs::new();
    for row in rows {
        let (name, amount) = row?;
        let total = out.entry(name).or_default();
        *total = total
            .checked_add(amount)
            .ok_or_else(|| bad("held amount overflow"))?;
    }
    Ok(out)
}
pub fn fits(conn: &Connection, project: ProjectId, needs: &Needs) -> Result<Fit> {
    let resources = declarations(conn, project)?;
    let holds = held(conn, project)?;
    let mut blocked = vec![];
    let mut reasons = vec![];
    for (name, amount) in needs {
        integer(*amount)?;
        if *amount == 0 {
            continue;
        }
        let taken = holds.get(name).copied().unwrap_or(0);
        let why = match resources.get(name) {
            None => Some("not declared".into()),
            Some(Resource { capacity: None, .. }) => Some("capacity unknown".into()),
            Some(Resource {
                capacity: Some(cap),
                ..
            }) if *amount > cap.saturating_sub(taken) => Some(format!("{taken}/{cap} held")),
            _ => None,
        };
        if let Some(why) = why {
            blocked.push(name.clone());
            reasons.push(format!("{name} {amount} ({why})"));
        }
    }
    Ok(Fit {
        blocked,
        reason: if reasons.is_empty() {
            String::new()
        } else {
            format!("needs {}", reasons.join(", "))
        },
    })
}

struct Run {
    project: ProjectId,
    step: Option<StepId>,
    group: String,
    generation: i64,
    work: i64,
    finished: bool,
    phase: String,
    cancelled: bool,
    priority: i64,
    current: bool,
}
fn run(conn: &Connection, run_id: RunId) -> Result<Run> {
    let raw = conn.query_row("SELECT r.project_id,r.step_id,r.generation,r.work_generation,r.item_index,r.finished_at IS NOT NULL,a.phase,a.cancel_requested,a.inputs_hash FROM runs r JOIN attempts a ON a.attempt_id=r.attempt_id WHERE r.run_id=?", [run_id.to_string()], |r| Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,bool>(5)?,r.get::<_,String>(6)?,r.get::<_,bool>(7)?,r.get::<_,String>(8)?))).optional()?.ok_or_else(|| bad("no such run"))?;
    let (project, step, generation, work, item, finished, phase, cancelled, hash) = raw;
    let project: ProjectId = id(project.ok_or_else(|| bad("leases require a project run"))?)?;
    let step: Option<StepId> = step.map(id).transpose()?;
    let (priority, current) = if let Some(step) = &step {
        let current = conn.query_row("SELECT generation,work_generation,declaration FROM steps WHERE project_id=?1 AND step_id=?2", params![project.to_string(), step.as_str()], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?))).optional()?;
        if let Some((g, w, decl)) = current {
            let decl: Value = serde_json::from_str(&decl)?;
            (
                decl.get("priority").and_then(Value::as_i64).unwrap_or(0),
                g == generation && w == work,
            )
        } else {
            (0, false)
        }
    } else {
        let current = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM calls c JOIN runs r ON r.run_id=c.run_id WHERE r.run_id=?1 AND c.call_id=r.run_id AND c.project_id=r.project_id AND c.status='running')",
            [run_id.to_string()], |r| r.get(0),
        )?;
        (0, current)
    };
    let group = if item >= 0 {
        format!(
            "{project}/{}/{generation}/{work}/{hash}",
            step.as_ref()
                .ok_or_else(|| bad("scatter run needs a step"))?
        )
    } else {
        run_id.to_string()
    };
    Ok(Run {
        project,
        step,
        group,
        generation,
        work,
        finished,
        phase,
        cancelled,
        priority,
        current,
    })
}

/// Atomic all-or-nothing reservation. Scatter items share their frozen group.
/// A replay by a live item returns its original rows and never reholds them.
pub fn hold_needs(
    tx: &mut WriteTransaction<'_>,
    run_id: RunId,
    needs: &Needs,
) -> Result<Vec<LeaseId>> {
    let run = run(tx.sql(), run_id)?;
    if run.finished || run.phase == "terminal" || run.cancelled || !run.current {
        return Err(conflict("run cannot reserve needs"));
    }
    let existing = need_rows(tx.sql(), &run.group)?;
    if !existing.is_empty() {
        let bundle: Needs = existing
            .iter()
            .map(|(_, name, amount)| (name.clone(), *amount))
            .collect();
        if &bundle != needs {
            return Err(conflict(
                "needs bundle differs from the original reservation",
            ));
        }
        return Ok(existing.into_iter().map(|(id, _, _)| id).collect());
    }
    let resources = declarations(tx.sql(), run.project)?;
    for (name, amount) in needs {
        integer(*amount)?;
        if !resources.contains_key(name) {
            return Err(bad(format!("project declares no resource '{name}'")));
        }
    }
    let fit = fits(tx.sql(), run.project, needs)?;
    if !fit.fits() {
        return Err(conflict(fit.reason));
    }
    let mut ids = vec![];
    for (name, amount) in needs {
        tx.sql().execute("INSERT INTO leases(project_id,run_id,request_id,kind,group_id,scope,resource,amount,priority,state,grant_id,created_at,granted_at) VALUES (?1,?2,?3,'needs',?4,?1,?5,?6,?7,'held',?3,?8,?8)", params![run.project.to_string(), run_id.to_string(), format!("needs/{}/{name}", run.group), run.group, name, integer(*amount)?, run.priority, now()?])?;
        ids.push(LeaseId(tx.sql().last_insert_rowid()));
    }
    if !ids.is_empty() {
        changed(tx, run.project);
    }
    Ok(ids)
}
fn need_rows(conn: &Connection, group: &str) -> Result<Vec<(LeaseId, String, u64)>> {
    let mut stmt = conn.prepare("SELECT lease_id,resource,amount FROM leases WHERE kind='needs' AND group_id=? ORDER BY lease_id")?;
    Ok(stmt
        .query_map([group], |r| {
            Ok((LeaseId(r.get(0)?), r.get(1)?, r.get::<_, i64>(2)? as u64))
        })?
        .collect::<std::result::Result<_, _>>()?)
}

/// Only a proven-stopped run can release its group. Every admitted scatter item
/// must be stopped, including reservations that have not yet acquired a run row.
pub fn release_needs(tx: &mut WriteTransaction<'_>, run_id: RunId) -> Result<bool> {
    let run = run(tx.sql(), run_id)?;
    if !run.finished {
        return Ok(false);
    }
    if run.group != run_id.to_string() {
        let live: bool = tx.sql().query_row("SELECT EXISTS(SELECT 1 FROM attempts a LEFT JOIN runs r ON r.attempt_id=a.attempt_id WHERE a.project_id=?1 AND a.step_id=?2 AND a.generation=?3 AND a.work_generation=?4 AND a.inputs_hash=(SELECT inputs_hash FROM attempts WHERE attempt_id=(SELECT attempt_id FROM runs WHERE run_id=?5)) AND (a.phase<>'terminal' OR (r.run_id IS NOT NULL AND r.finished_at IS NULL)))", params![run.project.to_string(),run.step.as_ref().ok_or_else(|| bad("needs group requires a step"))?.to_string(),run.generation,run.work,run_id.to_string()], |r| r.get(0))?;
        if live {
            return Ok(false);
        }
    }
    let n = tx.sql().execute("UPDATE leases SET state='released',released_at=?2,release_id='needs/'||lease_id WHERE kind='needs' AND group_id=?1 AND state='held'", params![run.group, now()?])?;
    if n > 0 {
        changed(tx, run.project);
    }
    Ok(n > 0)
}

/// Fresh acquisition. Transport retries should use `request_lease_keyed` instead.
pub fn request_lease(
    tx: &mut WriteTransaction<'_>,
    run: RunId,
    resource: &str,
    amount: u64,
) -> Result<LeaseId> {
    request_lease_keyed(tx, run, resource, amount, &RunId::new().to_string())
}
/// Durable request identity survives explicit release; reacquire uses a new key.
pub fn request_lease_keyed(
    tx: &mut WriteTransaction<'_>,
    run_id: RunId,
    resource: &str,
    amount: u64,
    request_id: &str,
) -> Result<LeaseId> {
    integer(amount)?;
    if request_id.is_empty() {
        return Err(bad("lease request id is empty"));
    }
    let existing = tx
        .sql()
        .query_row(
            "SELECT lease_id,run_id,resource,amount,kind FROM leases WHERE request_id=?",
            [request_id],
            |r| {
                Ok((
                    LeaseId(r.get(0)?),
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? as u64,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    if let Some((lease, owner, name, n, kind)) = existing {
        if kind == "section" && owner == run_id.to_string() && name == resource && n == amount {
            return Ok(lease);
        }
        return Err(conflict("lease request id was used for another request"));
    }
    let run = run(tx.sql(), run_id)?;
    if run.finished || run.cancelled || !run.current || run.phase != "executing" {
        return Err(bad("lease request requires an executing project run"));
    }
    let resources = declarations(tx.sql(), run.project)?;
    let declared = resources
        .get(resource)
        .ok_or_else(|| bad(format!("project declares no resource '{resource}'")))?;
    if let Capacity::Fixed(cap) = declared.declaration
        && amount > cap
    {
        return Err(bad(format!(
            "{amount} of {resource} is more than its capacity {cap}"
        )));
    }
    tx.sql().execute("INSERT INTO leases(project_id,run_id,request_id,scope,resource,amount,priority,state,created_at) VALUES (?1,?2,?3,?1,?4,?5,?6,'waiting',?7)", params![run.project.to_string(), run_id.to_string(), request_id, resource, integer(amount)?, run.priority, now()?])?;
    let lease = LeaseId(tx.sql().last_insert_rowid());
    changed(tx, run.project);
    Ok(lease)
}

pub fn leases(conn: &Connection, project: ProjectId) -> Result<Vec<Lease>> {
    let mut stmt = conn.prepare("SELECT l.lease_id,l.run_id,r.step_id,l.resource,l.amount,coalesce(json_extract(s.declaration,'$.priority'),l.priority) AS current_priority,l.state,CASE WHEN l.state='held' THEN l.granted_at ELSE l.created_at END FROM leases l JOIN runs r ON r.run_id=l.run_id LEFT JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id AND s.generation=r.generation AND s.work_generation=r.work_generation WHERE l.project_id=? AND l.kind='section' AND l.state IN ('waiting','held') ORDER BY current_priority DESC,l.lease_id")?;
    let rows = stmt.query_map([project.to_string()], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)? as u64,
            r.get::<_, i64>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
        ))
    })?;
    let mut out = vec![];
    for row in rows {
        let (lease, run, step, resource, amount, priority, state, since) = row?;
        out.push(Lease {
            id: LeaseId(lease),
            run: id(run)?,
            step: step.map(id).transpose()?,
            resource,
            amount,
            priority,
            state: if state == "held" {
                LeaseState::Held
            } else {
                LeaseState::Waiting
            },
            since,
        });
    }
    Ok(out)
}
fn lease_record(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    lease: &Lease,
    state: LeaseState,
    reason: Option<String>,
) -> Result<()> {
    tx.append_record(
        Some(project),
        Event::StepLease {
            step: lease.step.clone(),
            run: lease.run,
            lease: lease.id,
            resource: lease.resource.clone(),
            amount: lease.amount,
            state,
            reason,
        },
    )?;
    Ok(())
}
/// Run before new step admission. Each grant becomes visible to the next fit.
/// Drain permits existing sections.
pub fn grant_leases(tx: &mut WriteTransaction<'_>, project: ProjectId) -> Result<Vec<LeaseId>> {
    let mut granted = vec![];
    for lease in leases(tx.sql(), project)? {
        if lease.state != LeaseState::Waiting {
            continue;
        }
        let run = run(tx.sql(), lease.run)?;
        if run.cancelled || run.finished || !run.current || run.phase == "terminal" {
            cancel_waiting(tx, lease.run)?;
            continue;
        }
        if fits(
            tx.sql(),
            project,
            &Needs::from([(lease.resource.clone(), lease.amount)]),
        )?
        .fits()
        {
            tx.sql().execute("UPDATE leases SET state='held',granted_at=?2,grant_id='section/'||lease_id WHERE lease_id=?1 AND state='waiting'", params![lease.id.0,now()?])?;
            lease_record(tx, project, &lease, LeaseState::Held, None)?;
            changed(tx, project);
            granted.push(lease.id);
        }
    }
    Ok(granted)
}
/// Explicit voluntary release while alive; both identities must match. A section
/// release cannot touch automatic needs or any later acquisition.
pub fn release_lease(
    tx: &mut WriteTransaction<'_>,
    lease_id: LeaseId,
    run_id: RunId,
) -> Result<bool> {
    let project: Option<String> = tx.sql().query_row("SELECT project_id FROM leases WHERE lease_id=?1 AND run_id=?2 AND kind='section' AND state IN ('waiting','held')", params![lease_id.0,run_id.to_string()], |r| r.get(0)).optional()?.flatten();
    let Some(project) = project else {
        return Ok(false);
    };
    let project = id(project)?;
    let lease = leases(tx.sql(), project)?
        .into_iter()
        .find(|l| l.id == lease_id)
        .ok_or_else(|| {
            StoreError::InvalidDatabase("lease disappeared inside writer transaction".into())
        })?;
    finish_lease(tx, project, &lease, None)?;
    Ok(true)
}
fn finish_lease(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    lease: &Lease,
    reason: Option<String>,
) -> Result<()> {
    let state = if lease.state == LeaseState::Held {
        "released"
    } else {
        "cancelled"
    };
    tx.sql().execute("UPDATE leases SET state=?2,released_at=?3,release_id='section/'||lease_id WHERE lease_id=?1", params![lease.id.0,state,now()?])?;
    if lease.state == LeaseState::Held {
        lease_record(tx, project, lease, LeaseState::Released, reason)?;
    }
    changed(tx, project);
    Ok(())
}
/// Cancellation removes requests but retains granted leases until proven stop.
pub fn cancel_waiting(tx: &mut WriteTransaction<'_>, run_id: RunId) -> Result<usize> {
    let run = run(tx.sql(), run_id)?;
    let n = tx.sql().execute("UPDATE leases SET state='cancelled',released_at=?2 WHERE run_id=?1 AND kind='section' AND state='waiting'", params![run_id.to_string(),now()?])?;
    if n > 0 {
        changed(tx, run.project);
    }
    Ok(n)
}
/// Abandoned sections, unlike voluntary release, require durable stop proof.
pub fn release_stopped_run(tx: &mut WriteTransaction<'_>, run_id: RunId) -> Result<bool> {
    let run = run(tx.sql(), run_id)?;
    if !run.finished {
        return Ok(false);
    }
    let mut released = release_needs(tx, run_id)?;
    for lease in leases(tx.sql(), run.project)?
        .into_iter()
        .filter(|l| l.run == run_id)
    {
        finish_lease(tx, run.project, &lease, Some("its run ended".into()))?;
        released = true;
    }
    Ok(released)
}

/// Uses the model's readiness evaluator with one durable projection snapshot.
/// The supplied compiled plan must be the stored plan's revision `rev`, preventing stale
/// reads: a revision fixes the plan's rows.
pub fn admit_order(
    conn: &Connection,
    project: ProjectId,
    rev: Revision,
    plan: &Plan,
) -> Result<Vec<Admission>> {
    let (paused, deleted): (bool, bool) = conn.query_row(
        "SELECT paused,deleted_at IS NOT NULL FROM projects WHERE project_id=?",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mode: String =
        conn.query_row("SELECT mode FROM maintenance WHERE singleton=1", [], |r| {
            r.get(0)
        })?;
    if paused || deleted || mode != "normal" {
        return Ok(vec![]);
    }
    let stored: i64 = conn.query_row(
        "SELECT rev FROM plans WHERE project_id=?",
        [project.to_string()],
        |r| r.get(0),
    )?;
    if u64::try_from(stored).ok() != Some(rev.0) {
        return Err(conflict("compiled plan is stale"));
    }
    let mut state = StateSnapshot::default();
    let mut stmt =
        conn.prepare("SELECT name,value FROM inputs WHERE project_id=? AND value IS NOT NULL")?;
    for row in stmt.query_map([project.to_string()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (name, value) = row?;
        state.inputs.0.insert(name, serde_json::from_str(&value)?);
    }
    let mut stmt =
        conn.prepare("SELECT step_id,status,outputs,paused FROM steps WHERE project_id=?")?;
    let mut pauses = BTreeMap::new();
    for row in stmt.query_map([project.to_string()], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })? {
        let (step, status, outputs, paused) = row?;
        let step: StepId = id(step)?;
        pauses.insert(
            step.clone(),
            paused
                .as_deref()
                .is_some_and(|p| p != "false" && p != "null"),
        );
        state.steps.insert(
            step,
            StepState {
                status: serde_json::from_value(Value::String(status))?,
                outputs: outputs
                    .map(|s| serde_json::from_str(&s))
                    .transpose()?
                    .unwrap_or_default(),
                ..StepState::default()
            },
        );
    }
    state.paused = Pause::No;
    let decisions = readiness(plan, &state);
    let mut out = vec![];
    for step_id in plan.topological_order() {
        let step = &plan.steps()[step_id];
        if state.status(step_id) == StepStatus::Pending
            && !pauses.get(step_id).copied().unwrap_or(false)
            && !step.needs.is_empty()
            && matches!(decisions.get(step_id), Some(GateDecision::Ready))
        {
            out.push(Admission {
                step: step_id.clone(),
                needs: step.needs.iter().map(|(n, a)| (n.clone(), *a)).collect(),
                priority: step.priority,
            });
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.priority));
    Ok(out)
}
pub fn status(
    conn: &Connection,
    project: ProjectId,
    rev: Revision,
    plan: &Plan,
) -> Result<BTreeMap<String, ResourceStatus>> {
    let holds = held(conn, project)?;
    let sections = leases(conn, project)?;
    let ready = admit_order(conn, project, rev, plan)?;
    let mut queued = BTreeMap::<String, usize>::new();
    for step in ready {
        for resource in fits(conn, project, &step.needs)?.blocked {
            *queued.entry(resource).or_default() += 1;
        }
    }
    Ok(declarations(conn, project)?
        .into_iter()
        .map(|(name, resource)| {
            let row = ResourceStatus {
                resource,
                held: holds.get(&name).copied().unwrap_or(0),
                queued: queued.get(&name).copied().unwrap_or(0),
                holders: sections
                    .iter()
                    .filter(|l| l.resource == name && l.state == LeaseState::Held)
                    .cloned()
                    .collect(),
                waiting: sections
                    .iter()
                    .filter(|l| l.resource == name && l.state == LeaseState::Waiting)
                    .cloned()
                    .collect(),
            };
            (name, row)
        })
        .collect())
}

impl WriteTransaction<'_> {
    pub fn hold_needs(&mut self, run: RunId, needs: &Needs) -> Result<Vec<LeaseId>> {
        hold_needs(self, run, needs)
    }
    pub fn release_needs(&mut self, run: RunId) -> Result<bool> {
        release_needs(self, run)
    }
}
