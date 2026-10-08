//! Durable launch and completion protocol. External launch and cleanup occur
//! outside the writer; callbacks supply their evidence to these transactions.

use crate::{
    Result, StoreError, WriteTransaction,
    plans::{self, PlanContext, RetryMessages},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use sluice_model::{
    attempt::{
        AttemptNote, Ended, Finishing, GitSeen, PreviousAttempt, VALUE_CUT, cut_strings,
        short_release,
    },
    commands::{
        CompletionActionConflict, CompletionActionOutcome, CompletionActionTarget,
        RegisterCompletionAction, StepStatus, StepSubmit,
    },
    error::PublicError,
    events::Event,
    gates::{GateDecision, evaluate_step},
    hash::InputsHash,
    ids::{AttemptId, ProjectId, ResultId, RunId, StepGeneration, StepId, WorkGeneration},
    rpc::JsonMap,
    types::{Type, check_value_at},
};

/// Identity carried by every callback. A mismatch is a read-only no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptIdentity {
    pub project: ProjectId,
    pub step: StepId,
    pub generation: StepGeneration,
    pub work: WorkGeneration,
    pub attempt: AttemptId,
    pub run: RunId,
}
#[derive(Debug, Clone)]
pub struct Reserve {
    pub step: StepId,
    pub attempt: AttemptId,
    pub run: RunId,
    /// -1 for scalar work; every scatter item has its own reservation.
    pub item_index: i64,
    pub item_count: Option<u64>,
    pub inputs: JsonMap,
    pub inputs_hash: InputsHash,
    pub provenance: JsonMap,
    pub release_id: String,
    pub protocol_major: u16,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedRange {
    pub after: i64,
    pub through: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub identity: AttemptIdentity,
    pub prev_run: Option<RunId>,
    pub messages: AssignedRange,
}
#[derive(Debug, Clone)]
pub struct GuardianIdentity {
    pub unit_name: String,
    pub boot_id: String,
    pub pid: u32,
    pub start: String,
    pub cgroup: String,
    pub socket_challenge: String,
}

/// Implement using the resource and message owners' synchronous functions.
/// Hooks participate in the same transaction and must never perform external IO.
/// `assign` honors an exact scatter sibling range when supplied. Otherwise it
/// assigns every addressed message after cursor. An unstarted item supplies its
/// earlier cursor so that its preserved backlog and new feedback are both assigned.
pub trait ExecutionHooks: RetryMessages {
    fn assign(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        identity: &AttemptIdentity,
        cursor: i64,
        exact: Option<&AssignedRange>,
    ) -> Result<AssignedRange>;
    /// Called after executing phase and started_at are persisted in this transaction.
    /// Delegate cursor advancement to messages::advance_cursor; acknowledge invocation
    /// delivery separately. Hook errors must propagate so all start writes roll back.
    fn started(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        identity: &AttemptIdentity,
        range: &AssignedRange,
    ) -> Result<()>;
    fn hold(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        identity: &AttemptIdentity,
        needs: &[(String, u64)],
        scatter: bool,
    ) -> Result<()>;
    fn release(&mut self, tx: &mut WriteTransaction<'_>, identity: &AttemptIdentity) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompletionKind {
    Succeeded,
    Rejected { message: String },
    Failed(PublicError),
    Cancelled { message: String },
    Lost { message: String },
    Unknown { message: String },
}
#[derive(Debug, Clone)]
pub struct Complete {
    pub identity: AttemptIdentity,
    pub completion_id: String,
    pub kind: CompletionKind,
    pub outputs: JsonMap,
    /// Terminalization and hold release require proven process cleanup.
    pub processes_gone: bool,
    pub submission_version: Option<u64>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionResult {
    pub status: StepStatus,
    pub outputs: JsonMap,
    pub error: Option<PublicError>,
    pub result: Option<ResultId>,
    pub action: Option<CompletionActionOutcome>,
}

fn identity_from_row(
    project: ProjectId,
    step: StepId,
    attempt: AttemptId,
    run: RunId,
    generation: i64,
    work: i64,
) -> AttemptIdentity {
    AttemptIdentity {
        project,
        step,
        attempt,
        run,
        generation: StepGeneration(generation as u64),
        work: WorkGeneration(work as u64),
    }
}
fn current_callback(tx: &WriteTransaction<'_>, id: &AttemptIdentity) -> Result<Option<String>> {
    let row: Option<(String,String)> = tx.sql().query_row(
        "SELECT a.phase,s.run_ids FROM runs r JOIN attempts a ON a.attempt_id=r.attempt_id JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id
        WHERE r.run_id=?1 AND r.attempt_id=?2 AND r.project_id=?3 AND r.step_id=?4 AND r.generation=?5 AND r.work_generation=?6
        AND s.generation=r.generation AND s.work_generation=r.work_generation",
        params![id.run.to_string(),id.attempt.to_string(),id.project.to_string(),id.step.as_str(),plans::sql_counter(id.generation.0)?,plans::sql_counter(id.work.0)?],|r|Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    match row {
        Some((phase, runs)) if serde_json::from_str::<Vec<RunId>>(&runs)?.contains(&id.run) => {
            Ok(Some(phase))
        }
        _ => Ok(None),
    }
}
fn stored_reservation(
    tx: &WriteTransaction<'_>,
    request: &Reserve,
    project: ProjectId,
) -> Result<Option<Reservation>> {
    struct Stored {
        step: String,
        attempt: String,
        generation: i64,
        work: i64,
        prev: Option<String>,
        after: i64,
        through: i64,
        frozen: String,
        hash: String,
        index: i64,
        count: Option<i64>,
        release: String,
        protocol: u16,
    }
    let row = tx.sql().query_row(
        "SELECT r.step_id,r.attempt_id,r.generation,r.work_generation,r.prev_run,r.assigned_after,r.assigned_through,a.request,a.inputs_hash,r.item_index,json_extract(a.request,'$.item_count'),r.release_id,r.protocol_major FROM runs r JOIN attempts a USING(attempt_id) WHERE r.run_id=?1 AND r.project_id=?2",
        params![request.run.to_string(),project.to_string()], |r|Ok(Stored {step:r.get(0)?,attempt:r.get(1)?,generation:r.get(2)?,work:r.get(3)?,prev:r.get(4)?,after:r.get(5)?,through:r.get(6)?,frozen:r.get(7)?,hash:r.get(8)?,index:r.get(9)?,count:r.get(10)?,release:r.get(11)?,protocol:r.get(12)?}),
    ).optional()?;
    let Some(Stored {
        step,
        attempt,
        generation,
        work,
        prev,
        after,
        through,
        frozen,
        hash,
        index,
        count,
        release,
        protocol,
    }) = row
    else {
        return Ok(None);
    };
    let frozen: Value = serde_json::from_str(&frozen)?;
    if step != request.step.as_str()
        || attempt != request.attempt.to_string()
        || hash != request.inputs_hash.to_string()
        || frozen["inputs"] != serde_json::to_value(&request.inputs)?
        || frozen["provenance"] != serde_json::to_value(&request.provenance)?
        || index != request.item_index
        || count != request.item_count.map(plans::sql_counter).transpose()?
        || release != request.release_id
        || protocol != request.protocol_major
    {
        return Err(plans::conflict(
            "reservation identity reused with different request",
        ));
    }
    Ok(Some(Reservation {
        identity: identity_from_row(
            project,
            request.step.clone(),
            request.attempt,
            request.run,
            generation,
            work,
        ),
        prev_run: prev
            .map(|s| {
                s.parse()
                    .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))
            })
            .transpose()?,
        messages: AssignedRange { after, through },
    }))
}

pub fn reserve(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    request: Reserve,
    hooks: &mut impl ExecutionHooks,
) -> Result<Reservation> {
    if let Some(existing) = stored_reservation(tx, &request, context.project)? {
        return Ok(existing);
    }
    let mode: String =
        tx.sql()
            .query_row("SELECT mode FROM maintenance WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
    if mode != "normal" {
        return Err(plans::conflict("maintenance fences new reservations"));
    }
    plans::check_context(tx, context)?;
    let step = context
        .plan
        .steps()
        .get(&request.step)
        .ok_or_else(|| plans::invalid("no such step"))?;
    if step.is_external() {
        return Err(plans::invalid("external steps never execute"));
    }
    let state = plans::read_state(tx.sql(), context.project)?;
    let scatter = step.scatter.is_some();
    if !matches!(state.status(&request.step), StepStatus::Pending)
        && !(scatter && state.status(&request.step) == StepStatus::Running)
    {
        return Err(plans::invalid("step is not pending"));
    }
    if !matches!(
        evaluate_step(&context.plan, &state, step),
        GateDecision::Ready
    ) {
        return Err(plans::invalid("step is not ready"));
    }
    let effective = plans::effective_map(&context.plan, &state, step)?
        .ok_or_else(|| plans::invalid("inputs unavailable"))?;
    if sluice_model::plan::inputs_hash(&context.plan, &state, step) != Some(request.inputs_hash) {
        return Err(plans::conflict(
            "effective inputs changed before reservation",
        ));
    }
    if request.protocol_major == 0 || request.release_id.is_empty() {
        return Err(plans::invalid("release and protocol are required"));
    }
    let (generation,work,cursor,old_hash,old_total,instances): (i64,i64,i64,Option<String>,Option<i64>,String) = tx.sql().query_row("SELECT generation,work_generation,delivery_cursor,inputs_hash,total,instances FROM steps WHERE project_id=?1 AND step_id=?2",params![context.project.to_string(),request.step.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    if scatter {
        let name = step.scatter.as_ref().expect("scatter checked");
        let count = effective
            .0
            .get(name)
            .and_then(|v| v.as_value().as_array())
            .map(|a| a.len() as u64)
            .ok_or_else(|| plans::invalid("scatter binding is not an array"))?;
        if request.item_count != Some(count)
            || request.item_index < 0
            || request.item_index as u64 >= count
        {
            return Err(plans::invalid("scatter item identity does not match batch"));
        }
    } else if request.item_index != -1 || request.item_count.is_some() {
        return Err(plans::invalid("scalar item index must be -1"));
    }
    // Freeze schema as well as values; later registry edits cannot change this run's contract.
    let returns: Value = step
        .signature
        .outputs
        .iter()
        .map(|(n, t)| Ok((n.clone(), serde_json::to_value(t)?)))
        .collect::<serde_json::Result<serde_json::Map<_, _>>>()?
        .into();
    let declared: Value = step
        .declared_outputs
        .iter()
        .map(|(n, t)| Ok((n.clone(), serde_json::to_value(&t.ty)?)))
        .collect::<serde_json::Result<serde_json::Map<_, _>>>()?
        .into();
    validate_frozen_inputs(step, &request.inputs)?;
    for (name, value) in &effective.0 {
        if matches!(
            step.bindings.get(name),
            Some(sluice_model::plan::Binding::File(_))
        ) {
            continue;
        }
        let expected = if step.scatter.as_ref() == Some(name) {
            value
                .as_value()
                .as_array()
                .and_then(|a| a.get(request.item_index as usize))
                .ok_or_else(|| plans::invalid("scatter index missing"))?
        } else {
            value.as_value()
        };
        if request.inputs.0.get(name).is_none_or(|actual| {
            !sluice_model::hash::data_equal(actual.as_value(), expected).unwrap_or(false)
        }) {
            return Err(plans::conflict(format!(
                "frozen input {name} differs from effective binding"
            )));
        }
    }
    // A fn that takes `listen` (every agent fn, and a fn that runs one and passes it on)
    // takes messages on its live feed unless this run's frozen inputs turn it off.
    let listens = step.signature.inputs.contains_key("listen")
        && request.inputs.0.get("listen").map(|v| v.as_value()) != Some(&Value::Bool(false));
    let frozen = json!({"declaration":plans::wire_step(&context.plan,&step.id)?,"inputs":request.inputs,"effective_inputs":effective,"returns":returns,"declared":declared,"item_count":request.item_count,"provenance":request.provenance,"listens":listens});
    let compatible = !scatter
        || old_hash.as_deref() == Some(&request.inputs_hash.to_string())
            && old_total == request.item_count.map(plans::sql_counter).transpose()?;
    let mut instances: Value = if compatible {
        serde_json::from_str(&instances)?
    } else {
        json!({})
    };
    let index = request.item_index.to_string();
    if instances
        .get(&index)
        .is_some_and(|v| v["status"] == "succeeded")
    {
        return Err(plans::invalid("successful scatter item is retained"));
    }
    let predecessor: Option<(String,Option<String>,i64,i64,String)> = tx.sql().query_row(
        "SELECT r.run_id,r.started_at,r.assigned_after,r.assigned_through,a.request FROM runs r JOIN attempts a USING(attempt_id)
        WHERE r.project_id=?1 AND r.step_id=?2 AND r.generation=?3 AND r.item_index=?4 AND a.phase='terminal' ORDER BY r.created_at DESC,r.run_id DESC LIMIT 1",
        params![context.project.to_string(),request.step.as_str(),generation,request.item_index],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    ).optional()?;
    let predecessor = predecessor.filter(|(_, _, _, _, prior)| {
        if !scatter {
            return true;
        }
        serde_json::from_str::<Value>(prior).is_ok_and(|prior| {
            prior["item_count"] == frozen["item_count"]
                && prior["effective_inputs"] == frozen["effective_inputs"]
                && prior["inputs"] == frozen["inputs"]
        })
    });
    let prev_run: Option<RunId> = predecessor
        .as_ref()
        .map(|(run, _, _, _, _)| {
            run.parse()
                .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))
        })
        .transpose()?;
    let sibling: Option<(i64, i64)> = if scatter {
        tx.sql().query_row("SELECT assigned_after,assigned_through FROM runs WHERE project_id=?1 AND step_id=?2 AND generation=?3 AND work_generation=?4 ORDER BY created_at,run_id LIMIT 1",params![context.project.to_string(),request.step.as_str(),generation,work],|r|Ok((r.get(0)?,r.get(1)?))).optional()?
    } else {
        None
    };
    let replay = predecessor
        .as_ref()
        .filter(|(_, started, _, _, _)| scatter && started.is_none())
        .map(|(_, _, after, through, _)| AssignedRange {
            after: *after,
            through: *through,
        });
    let cursor = replay.as_ref().map_or(cursor, |range| range.after);
    let exact = sibling.map(|(after, through)| AssignedRange {
        after: replay
            .as_ref()
            .map_or(after, |range| after.min(range.after)),
        through: replay
            .as_ref()
            .map_or(through, |range| through.max(range.through)),
    });
    let id = identity_from_row(
        context.project,
        request.step.clone(),
        request.attempt,
        request.run,
        generation,
        work,
    );
    let at = plans::now()?;
    tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,item_index,phase,request,inputs_hash,provenance,unit,created_at) VALUES (?1,?2,?3,?4,?5,?6,'reserved',?7,?8,?9,?10,?11)",
        params![id.attempt.to_string(),id.project.to_string(),id.step.as_str(),generation,work,request.item_index,frozen.to_string(),request.inputs_hash.to_string(),serde_json::to_string(&request.provenance)?,step.unit_name().to_string(),at])?;
    tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,item_index,prev_run,unit,release_id,protocol_major,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![id.run.to_string(),id.project.to_string(),id.attempt.to_string(),id.step.as_str(),generation,work,request.item_index,prev_run.map(|r|r.to_string()),step.unit_name().to_string(),request.release_id,request.protocol_major,at])?;
    let range = hooks.assign(tx, &id, cursor, exact.as_ref())?;
    if range.after < 0
        || range.after != exact.as_ref().map_or(cursor, |range| range.after)
        || replay
            .as_ref()
            .is_some_and(|prior| range.after > prior.after || range.through < prior.through)
        || range.through < range.after
        || exact.as_ref().is_some_and(|expected| *expected != range)
    {
        return Err(plans::invalid(
            "message assignment returned an invalid range",
        ));
    }
    tx.sql().execute(
        "UPDATE runs SET assigned_after=?2,assigned_through=?3 WHERE run_id=?1",
        params![id.run.to_string(), range.after, range.through],
    )?;
    hooks.hold(
        tx,
        &id,
        &step
            .needs
            .iter()
            .map(|(n, v)| (n.clone(), *v))
            .collect::<Vec<_>>(),
        scatter,
    )?;
    instances[&index] = json!({"status":"running","run":id.run,"inputs":request.inputs});
    let runs: Vec<RunId> = if scatter {
        (0..request.item_count.expect("validated scatter count"))
            .filter_map(|i| instances.get(i.to_string()).and_then(|v| v.get("run")))
            .map(|v| serde_json::from_value(v.clone()))
            .collect::<serde_json::Result<_>>()?
    } else {
        vec![id.run]
    };
    let done = if scatter {
        instances
            .as_object()
            .expect("instances object")
            .values()
            .filter(|v| v["status"] == "succeeded" || v["status"] == "failed")
            .count() as i64
    } else {
        0
    };
    tx.sql().execute("UPDATE steps SET status='running',inputs_hash=?3,run_ids=?4,instances=?5,total=?6,done=?7,error=NULL,skipped=NULL,manual=0,progress=NULL,progress_at=NULL,progress_run=NULL WHERE project_id=?1 AND step_id=?2",
        params![id.project.to_string(),id.step.as_str(),request.inputs_hash.to_string(),serde_json::to_string(&runs)?,if scatter {instances.to_string()} else {"{}".into()},request.item_count.map(plans::sql_counter).transpose()?,done])?;
    plans::status_record(
        tx,
        id.project,
        &id.step,
        state.status(&id.step),
        StepStatus::Running,
        None,
    )?;
    Ok(Reservation {
        identity: id,
        prev_run,
        messages: range,
    })
}
fn validate_frozen_inputs(step: &sluice_model::plan::Step, inputs: &JsonMap) -> Result<()> {
    for (name, ty) in step.signature.inputs.iter().chain(step.extra_inputs.iter()) {
        match inputs.0.get(name) {
            Some(value) => check_value_at(ty, value.as_value(), &format!("inputs.{name}"))
                .map_err(|e| {
                    plans::invalid(
                        e.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join("; "),
                    )
                })?,
            None if matches!(ty, Type::Optional(_)) => {}
            None => return Err(plans::invalid(format!("inputs.{name}: required"))),
        }
    }
    for name in inputs.0.keys() {
        if !step.signature.inputs.contains_key(name) && !step.extra_inputs.contains_key(name) {
            return Err(plans::invalid(format!("inputs.{name}: unknown input")));
        }
    }
    Ok(())
}

pub fn claim(
    tx: &mut WriteTransaction<'_>,
    id: &AttemptIdentity,
    guardian: GuardianIdentity,
) -> Result<bool> {
    if current_callback(tx, id)?.as_deref() != Some("reserved") {
        return Ok(false);
    }
    if guardian.pid == 0
        || guardian.unit_name.is_empty()
        || guardian.boot_id.is_empty()
        || guardian.start.is_empty()
        || guardian.cgroup.is_empty()
        || guardian.socket_challenge.is_empty()
    {
        return Err(plans::invalid("guardian identity is incomplete"));
    }
    let changed = tx.sql().execute("UPDATE attempts SET phase='claimed',spawn_attempted=1 WHERE attempt_id=?1 AND phase='reserved' AND cancel_requested=0",[id.attempt.to_string()])?;
    if changed == 0 {
        return Ok(false);
    }
    tx.sql().execute("UPDATE runs SET unit_name=?2,boot_id=?3,guardian_pid=?4,guardian_start=?5,cgroup=?6,socket_challenge=?7 WHERE run_id=?1",params![id.run.to_string(),guardian.unit_name,guardian.boot_id,guardian.pid,guardian.start,guardian.cgroup,guardian.socket_challenge])?;
    plans::status_record(
        tx,
        id.project,
        &id.step,
        StepStatus::Running,
        StepStatus::Running,
        None,
    )?;
    Ok(true)
}
pub fn spawn_attempted(tx: &mut WriteTransaction<'_>, id: &AttemptIdentity) -> Result<bool> {
    if current_callback(tx, id)?.as_deref() != Some("reserved") {
        return Ok(false);
    }
    let changed = tx.sql().execute("UPDATE attempts SET spawn_attempted=1 WHERE attempt_id=?1 AND spawn_attempted=0 AND cancel_requested=0",[id.attempt.to_string()])?;
    if changed > 0 {
        plans::status_record(
            tx,
            id.project,
            &id.step,
            StepStatus::Running,
            StepStatus::Running,
            None,
        )?;
    }
    Ok(changed > 0)
}
pub fn started(
    tx: &mut WriteTransaction<'_>,
    id: &AttemptIdentity,
    hooks: &mut impl ExecutionHooks,
) -> Result<bool> {
    if current_callback(tx, id)?.as_deref() != Some("claimed") {
        return Ok(false);
    }
    let cancel: bool = tx.sql().query_row(
        "SELECT cancel_requested FROM attempts WHERE attempt_id=?1",
        [id.attempt.to_string()],
        |r| r.get(0),
    )?;
    if cancel {
        return Ok(false);
    }
    let range = tx.sql().query_row(
        "SELECT assigned_after,assigned_through FROM runs WHERE run_id=?1",
        [id.run.to_string()],
        |r| {
            Ok(AssignedRange {
                after: r.get(0)?,
                through: r.get(1)?,
            })
        },
    )?;
    tx.sql().execute(
        "UPDATE attempts SET phase='executing' WHERE attempt_id=?1",
        [id.attempt.to_string()],
    )?;
    tx.sql().execute(
        "UPDATE runs SET started_at=?2 WHERE run_id=?1",
        params![id.run.to_string(), plans::now()?],
    )?;
    hooks.started(tx, id, &range)?;
    plans::status_record(
        tx,
        id.project,
        &id.step,
        StepStatus::Running,
        StepStatus::Running,
        None,
    )?;
    Ok(true)
}
pub fn cancel(
    tx: &mut WriteTransaction<'_>,
    id: &AttemptIdentity,
    author: String,
    reason: String,
) -> Result<bool> {
    if current_callback(tx, id)?.is_none_or(|phase| phase == "terminal") {
        return Ok(false);
    }
    let changed = tx.sql().execute(
        "UPDATE attempts SET cancel_requested=1 WHERE attempt_id=?1 AND cancel_requested=0",
        [id.attempt.to_string()],
    )?;
    if changed > 0 {
        tx.append_record(
            Some(id.project),
            Event::StepCancel {
                step: id.step.clone(),
                author,
                reason,
            },
        )?;
        tx.changed(Some(id.project), "status");
    }
    Ok(changed > 0)
}

fn check_schema(schema: &Value, outputs: &JsonMap) -> Result<()> {
    let schema = schema
        .as_object()
        .ok_or_else(|| plans::invalid("invalid frozen result schema"))?;
    let mut errors = vec![];
    for (name, ty) in schema {
        let ty = Type::parse(ty).map_err(|e| plans::invalid(e.to_string()))?;
        match outputs.0.get(name) {
            Some(value) => {
                if let Err(e) = check_value_at(&ty, value.as_value(), &format!("outputs.{name}")) {
                    errors.extend(e.into_iter().map(|e| e.to_string()));
                }
            }
            None if matches!(ty, Type::Optional(_)) => {}
            None => errors.push(format!("outputs.{name}: required")),
        }
    }
    for name in outputs.0.keys() {
        if !schema.contains_key(name) {
            errors.push(format!("outputs.{name}: undeclared output"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(PublicError::Invalid {
            message: "outputs do not match frozen declarations".into(),
            errors,
        }
        .into())
    }
}
pub fn step_submit(tx: &mut WriteTransaction<'_>, request: StepSubmit) -> Result<Option<u64>> {
    let row: Option<(String,i64,i64,String)> = tx.sql().query_row("SELECT r.attempt_id,r.generation,r.work_generation,a.request FROM runs r JOIN attempts a USING(attempt_id) WHERE r.run_id=?1 AND r.project_id=?2 AND r.step_id=?3 AND r.finished_at IS NULL",params![request.run.to_string(),request.project.to_string(),request.step.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((attempt, generation, work, frozen)) = row else {
        return Ok(None);
    };
    let attempt = attempt
        .parse()
        .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?;
    let id = identity_from_row(
        request.project,
        request.step.clone(),
        attempt,
        request.run,
        generation,
        work,
    );
    if !matches!(
        current_callback(tx, &id)?.as_deref(),
        Some("reserved" | "claimed" | "executing")
    ) {
        return Ok(None);
    }
    let frozen: Value = serde_json::from_str(&frozen)?;
    if frozen["declared"].as_object().is_none_or(|o| o.is_empty()) {
        return Err(plans::invalid("step declares no submitted outputs"));
    }
    // Submitting ends the agent's session, so a run submits once.
    let submitted: bool = tx.sql().query_row(
        "SELECT EXISTS(SELECT 1 FROM submissions WHERE run_id=?1)",
        [id.run.to_string()],
        |r| r.get(0),
    )?;
    if submitted {
        return Err(plans::conflict(format!(
            "run {} of step {} already submitted its outputs, and submitting ended its session",
            id.run, id.step
        )));
    }
    check_schema(&frozen["declared"], &request.outputs)?;
    tx.sql().execute(
        "INSERT INTO submissions(run_id,project_id,step_id,outputs,at) VALUES (?1,?2,?3,?4,?5)",
        params![
            id.run.to_string(),
            id.project.to_string(),
            id.step.as_str(),
            serde_json::to_string(&request.outputs)?,
            plans::now()?
        ],
    )?;
    let version: i64 = 1;
    tx.append_record(
        Some(id.project),
        Event::StepSubmit {
            step: request.step,
            run: request.run,
            outputs: request.outputs,
            author: request.author,
        },
    )?;
    tx.changed(Some(id.project), "status");
    Ok(Some(version as u64))
}

/// `step_progress`: merge `request.outputs` over the latest values the step's current run has
/// published, each field one of the run's frozen outputs (its fn's or declared) and of that
/// type. Refused (`conflict`) unless the step is running and `request.run` is its current run;
/// `invalid` for a scattered step, an unknown field or a value that does not fit. Progress
/// is store state only: it writes no record, so no wait wakes on it, and nothing reads it as a
/// value (inputs, handoffs, gates and completion read `outputs`). Returns `{project, step,
/// run, progress, at}`.
pub fn step_progress(
    tx: &mut WriteTransaction<'_>,
    request: sluice_model::commands::StepProgress,
) -> Result<Value> {
    let project = crate::projects::resolve(tx.sql(), &request.project)?.project_id;
    let step = request.step;
    // status, run_ids, total, progress, progress_run
    type Row = (String, String, Option<i64>, Option<String>, Option<String>);
    let row: Option<Row> = tx
        .sql()
        .query_row(
            "SELECT status,run_ids,total,progress,progress_run FROM steps WHERE project_id=?1 AND step_id=?2",
            params![project.to_string(), step.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((status, runs, total, progress, progress_run)) = row else {
        return Err(PublicError::NotFound {
            message: format!("no step {step} in the plan"),
        }
        .into());
    };
    if status != "running" {
        return Err(plans::conflict(format!(
            "step {step} is {status}, not running: only a running step's current run publishes progress"
        )));
    }
    if total.is_some() {
        return Err(plans::invalid(format!(
            "step {step} is scattered: progress is for a step that runs once"
        )));
    }
    let run = request.run;
    let frozen: Option<String> = tx
        .sql()
        .query_row(
            "SELECT a.request FROM runs r JOIN attempts a USING(attempt_id)
             WHERE r.run_id=?1 AND r.project_id=?2 AND r.step_id=?3 AND r.finished_at IS NULL
             AND a.phase IN ('reserved','claimed','executing')",
            params![run.to_string(), project.to_string(), step.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let current = serde_json::from_str::<Vec<RunId>>(&runs)?.contains(&run);
    let Some(frozen) = frozen.filter(|_| current) else {
        return Err(plans::conflict(format!(
            "run {run} is not the current run of step {step}"
        )));
    };
    if request.outputs.0.is_empty() {
        return Err(plans::invalid("outputs: give at least one field"));
    }
    let frozen: Value = serde_json::from_str(&frozen)?;
    let mut errors = vec![];
    for (name, value) in &request.outputs.0 {
        let ty = frozen["declared"]
            .get(name)
            .or_else(|| frozen["returns"].get(name));
        let Some(ty) = ty else {
            errors.push(format!("outputs.{name}: not an output of step {step}"));
            continue;
        };
        let ty = Type::parse(ty).map_err(|e| plans::invalid(e.to_string()))?;
        if let Err(e) = check_value_at(&ty, value.as_value(), &format!("outputs.{name}")) {
            errors.extend(e.into_iter().map(|e| e.to_string()));
        }
    }
    if !errors.is_empty() {
        return Err(PublicError::Invalid {
            message: "progress does not fit the step's outputs".into(),
            errors,
        }
        .into());
    }
    // Earlier progress of this same run, merged under the new fields.
    let mut merged = match (progress, progress_run) {
        (Some(progress), Some(by)) if by == run.to_string() => {
            serde_json::from_str::<serde_json::Map<String, Value>>(&progress)?
        }
        _ => serde_json::Map::new(),
    };
    for (name, value) in request.outputs.0 {
        merged.insert(name, value.as_value().clone());
    }
    let at = plans::now()?;
    let merged = Value::Object(merged);
    tx.sql().execute(
        "UPDATE steps SET progress=?3,progress_at=?4,progress_run=?5 WHERE project_id=?1 AND step_id=?2",
        params![
            project.to_string(),
            step.as_str(),
            merged.to_string(),
            at,
            run.to_string()
        ],
    )?;
    // A view of its own: no wait (log, status) and no scheduler pass listens for it; the
    // dashboard redraws from its snapshots.
    tx.changed(Some(project), PROGRESS_VIEW);
    Ok(json!({"project":project,"step":step,"run":run,"progress":merged,"at":at}))
}

/// The change view `step_progress` touches; the scheduler's dirty check leaves it out.
pub const PROGRESS_VIEW: &str = "progress";

/// A step's progress as the dashboard shows it (`step_progress`).
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub outputs: serde_json::Map<String, Value>,
    /// When it was last set (RFC 3339).
    pub at: String,
    pub run: Option<String>,
    /// The step is running, so the progress is its current run's: live.
    pub live: bool,
    /// The step has finished with outputs (succeeded, or stale since) after the progress was
    /// set: the outputs are the fresher values and the progress is history.
    pub superseded: bool,
}
impl Progress {
    /// The freshest value of `field`: the progress value unless the outputs supersede it.
    pub fn fresher(&self, field: &str) -> Option<&Value> {
        (!self.superseded)
            .then(|| self.outputs.get(field))
            .flatten()
    }
}

/// Read a step's progress, `None` when it has none.
pub fn read_progress(
    c: &rusqlite::Connection,
    project: ProjectId,
    step: &str,
) -> Result<Option<Progress>> {
    // status, progress, progress_at, progress_run, the result's recorded_at
    type Row = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<Row> = c
        .query_row(
            "SELECT s.status,s.progress,s.progress_at,s.progress_run,r.recorded_at FROM steps s
             LEFT JOIN step_results r ON r.result_id=s.result_id
             WHERE s.project_id=?1 AND s.step_id=?2",
            params![project.to_string(), step],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((status, Some(progress), Some(at), run, finished)) = row else {
        return Ok(None);
    };
    let time = |text: &str| {
        time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()
    };
    let superseded = matches!(status.as_str(), "succeeded" | "stale")
        && match (finished.as_deref().and_then(time), time(&at)) {
            (Some(finished), Some(set)) => finished >= set,
            (Some(_), None) => true,
            _ => false,
        };
    Ok(Some(Progress {
        outputs: serde_json::from_str(&progress)?,
        live: status == "running",
        at,
        run,
        superseded,
    }))
}

pub fn completion_target(
    tx: &WriteTransaction<'_>,
    project: ProjectId,
    step: &StepId,
) -> Result<Option<CompletionActionTarget>> {
    let row: Option<(i64,i64,Option<String>,Option<String>)> = tx.sql().query_row("SELECT s.generation,s.work_generation,s.result_id,r.attempt_id FROM steps s LEFT JOIN step_results r ON s.result_id=r.result_id WHERE s.project_id=?1 AND s.step_id=?2",params![project.to_string(),step.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((generation, work, Some(result), attempt)) = row else {
        return Ok(None);
    };
    Ok(Some(CompletionActionTarget {
        step: step.clone(),
        generation: StepGeneration(generation as u64),
        work: WorkGeneration(work as u64),
        result: result
            .parse()
            .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?,
        result_attempt: attempt
            .map(|s| {
                s.parse()
                    .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))
            })
            .transpose()?,
    }))
}

/// The store verifies and freezes the supplied target; repeat registration never
/// recaptures newer work. The helper obtains a target through completion_target.
pub fn register_completion_action(
    tx: &mut WriteTransaction<'_>,
    request: RegisterCompletionAction,
    messages: &impl RetryMessages,
) -> Result<bool> {
    let row: Option<(String,String,i64,i64,Option<String>)> = tx.sql().query_row("SELECT r.step_id,r.attempt_id,r.generation,r.work_generation,r.completion_action FROM runs r WHERE r.run_id=?1 AND r.project_id=?2",params![request.run.to_string(),request.project.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((step, attempt, generation, work, registered)) = row else {
        return Ok(false);
    };
    let value = json!({"target":request.target,"message":request.message,"author":request.author});
    if let Some(registered) = registered {
        let registered: Value = serde_json::from_str(&registered)?;
        if registered["target"] != value["target"] || registered["message"] != value["message"] {
            return Err(plans::conflict("completion action is already registered"));
        }
        return Ok(true);
    }
    let id = identity_from_row(
        request.project,
        step.parse()
            .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?,
        attempt
            .parse()
            .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?,
        request.run,
        generation,
        work,
    );
    if current_callback(tx, &id)?.is_none_or(|phase| phase == "terminal") {
        return Ok(false);
    }
    plans::validate_message(&request.message)?;
    messages.validate_retry(
        tx,
        request.project,
        std::slice::from_ref(&request.target.step),
        &request.message,
        &request.author,
    )?;
    if request.author.len() > 1024 {
        return Err(plans::invalid("completion action author is too long"));
    }
    if completion_target(tx, request.project, &request.target.step)?.as_ref()
        != Some(&request.target)
    {
        return Err(plans::conflict("completion action target changed"));
    }
    let status: String = tx.sql().query_row(
        "SELECT status FROM steps WHERE project_id=?1 AND step_id=?2",
        params![request.project.to_string(), request.target.step.as_str()],
        |r| r.get(0),
    )?;
    if !matches!(status.as_str(), "succeeded" | "failed" | "stale") {
        return Err(plans::invalid("completion action target is not retryable"));
    }
    tx.sql().execute(
        "UPDATE runs SET completion_action=?2 WHERE run_id=?1",
        params![request.run.to_string(), value.to_string()],
    )?;
    tx.append_record(
        Some(request.project),
        Event::RunCompletionActionRegistered {
            run: request.run,
            target: request.target,
            message: request.message,
            author: request.author,
        },
    )?;
    tx.changed(Some(request.project), "status");
    Ok(true)
}

fn completion_json(result: &CompletionResult) -> Value {
    json!({"status":result.status,"outputs":result.outputs,"error":result.error,"result":result.result,"action":result.action})
}
fn parse_completion(value: &str) -> Result<CompletionResult> {
    let value: Value = serde_json::from_str(value)?;
    Ok(CompletionResult {
        status: serde_json::from_value(value["status"].clone())?,
        outputs: serde_json::from_value(value["outputs"].clone())?,
        error: serde_json::from_value(value["error"].clone())?,
        result: serde_json::from_value(value["result"].clone())?,
        action: serde_json::from_value(value["action"].clone())?,
    })
}

/// Terminal settlement uses admission data; reconciliation uses current data.
/// A registry/plan error cannot undo the result or cleanup-backed hold release.
pub struct CompletionContext<'a> {
    pub admitted: &'a PlanContext,
    pub current: std::result::Result<&'a PlanContext, &'a str>,
}
pub fn complete(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    request: Complete,
    hooks: &mut impl ExecutionHooks,
) -> Result<Option<CompletionResult>> {
    complete_frozen(
        tx,
        CompletionContext {
            admitted: context,
            current: Ok(context),
        },
        request,
        hooks,
    )
}
pub fn complete_frozen(
    tx: &mut WriteTransaction<'_>,
    completion: CompletionContext<'_>,
    request: Complete,
    hooks: &mut impl ExecutionHooks,
) -> Result<Option<CompletionResult>> {
    let context = completion.admitted;
    let id = &request.identity;
    let prior: Option<(Option<String>,Option<String>)> = tx.sql().query_row("SELECT completion_id,result FROM runs WHERE run_id=?1 AND attempt_id=?2 AND project_id=?3 AND step_id=?4 AND generation=?5 AND work_generation=?6",params![id.run.to_string(),id.attempt.to_string(),id.project.to_string(),id.step.as_str(),plans::sql_counter(id.generation.0)?,plans::sql_counter(id.work.0)?],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((Some(completion), Some(result))) = prior {
        if completion == request.completion_id {
            return Ok(Some(parse_completion(&result)?));
        }
        return Ok(None);
    }
    if current_callback(tx, id)?.is_none_or(|phase| phase == "terminal") {
        return Ok(None);
    }
    if context.project != id.project {
        return Err(plans::invalid("completion project mismatch"));
    }
    if let Ok(current) = completion.current {
        if current.project != id.project {
            return Err(plans::invalid("reconciliation project mismatch"));
        }
        plans::check_context(tx, current)?;
    }
    if request.completion_id.is_empty() || request.completion_id.len() > 256 {
        return Err(plans::invalid("completion id must contain 1 to 256 bytes"));
    }
    if !request.processes_gone {
        return Err(plans::invalid(
            "process cleanup must be proven before terminalization",
        ));
    }
    let (frozen,index,registered,cancelled): (String,i64,Option<String>,bool) = tx.sql().query_row("SELECT a.request,a.item_index,r.completion_action,a.cancel_requested FROM runs r JOIN attempts a USING(attempt_id) WHERE r.run_id=?1",[id.run.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let frozen: Value = serde_json::from_str(&frozen)?;
    let submission: Option<(i64, String)> = tx
        .sql()
        .query_row(
            "SELECT version,outputs FROM submissions WHERE run_id=?1",
            [id.run.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if request.submission_version != submission.as_ref().map(|(version, _)| *version as u64) {
        return Err(plans::conflict("completion submission version changed"));
    }
    let mut outputs = request.outputs;
    let mut kind = request.kind;
    // A run settled on its submission (step_settle) succeeds with the outputs recorded then,
    // unless it finished by itself first; its stop is not a cancel.
    let settle = frozen
        .get("settle")
        .filter(|v| v.is_object() && submission.is_some());
    if let Some(settle) = settle
        && !matches!(kind, CompletionKind::Succeeded)
    {
        outputs = serde_json::from_value(settle["outputs"].clone())?;
        kind = CompletionKind::Succeeded;
    }
    let cancelled = cancelled && settle.is_none();
    let submitted = submission.is_some();
    if let Some((_, submitted)) = submission {
        let submitted: JsonMap = serde_json::from_str(&submitted)?;
        // Returned values override submitted values, as the existing fn contract does.
        for (name, value) in submitted.0 {
            outputs.0.entry(name).or_insert(value);
        }
    }
    let mut error = if cancelled {
        Some(PublicError::Cancelled {
            message: "cancel requested".into(),
        })
    } else {
        match &kind {
            CompletionKind::Succeeded => None,
            CompletionKind::Rejected { message } => Some(PublicError::FnFailure {
                message: message.clone(),
            }),
            CompletionKind::Failed(error) => Some(error.clone()),
            CompletionKind::Cancelled { message } => Some(PublicError::Cancelled {
                message: message.clone(),
            }),
            CompletionKind::Lost { message } | CompletionKind::Unknown { message } => {
                Some(PublicError::ProcessLost {
                    message: message.clone(),
                })
            }
        }
    };
    let mut schema = frozen["returns"].clone();
    if let (Some(schema), Some(declared)) = (schema.as_object_mut(), frozen["declared"].as_object())
    {
        schema.extend(declared.clone());
    }
    for (name, ty) in frozen["declared"]
        .as_object()
        .ok_or_else(|| plans::invalid("frozen declared schema missing"))?
    {
        if matches!(
            Type::parse(ty).map_err(|e| plans::invalid(e.to_string()))?,
            Type::Optional(_)
        ) {
            outputs
                .0
                .entry(name.clone())
                .or_insert(Value::Null.try_into()?);
        }
    }
    // Required declared outputs nobody submitted (or returned): the work ended first.
    let mut unsubmitted = vec![];
    for (name, ty) in frozen["declared"]
        .as_object()
        .ok_or_else(|| plans::invalid("frozen declared schema missing"))?
    {
        if !outputs.0.contains_key(name)
            && !matches!(
                Type::parse(ty).map_err(|e| plans::invalid(e.to_string()))?,
                Type::Optional(_)
            )
        {
            unsubmitted.push(name.as_str());
        }
    }
    if !submitted && !unsubmitted.is_empty() && !cancelled {
        let session = || {
            outputs
                .0
                .get("session")
                .and_then(|v| v.as_value().as_str())
                .map(str::to_owned)
        };
        match &error {
            None => {
                error = Some(PublicError::ExitedWithoutSubmit {
                    message: format!(
                        "the run finished without submitting {}",
                        unsubmitted.join(", ")
                    ),
                    session: session(),
                });
            }
            Some(PublicError::AgentFailure {
                kind,
                message,
                session,
            }) if kind == "ExitedWithoutSubmit" => {
                error = Some(PublicError::ExitedWithoutSubmit {
                    message: message.clone(),
                    session: session.clone(),
                });
            }
            Some(_) => {}
        }
    }
    let validation = check_schema(&schema, &outputs);
    let rejected = !cancelled && matches!(kind, CompletionKind::Rejected { .. });
    if error.is_none()
        && let Err(validation) = validation
    {
        error = Some(validation.into_public(false));
    }
    let status = if error.is_none() {
        StepStatus::Succeeded
    } else {
        StepStatus::Failed
    };
    let at = plans::now()?;
    tx.sql().execute(
        "UPDATE attempts SET phase='terminal',finished_at=?2 WHERE attempt_id=?1",
        params![id.attempt.to_string(), at],
    )?;
    tx.sql().execute(
        "UPDATE runs SET completion_id=?2,finished_at=?3,completion_ack=1 WHERE run_id=?1",
        params![id.run.to_string(), request.completion_id, at],
    )?;
    let state = plans::read_state(tx.sql(), id.project)?;
    let mut step_status = status.clone();
    let mut step_outputs = outputs.clone();
    let mut step_error = error.clone();
    if index >= 0 {
        let (instances, total): (String, i64) = tx.sql().query_row(
            "SELECT instances,total FROM steps WHERE project_id=?1 AND step_id=?2",
            params![id.project.to_string(), id.step.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut instances: Value = serde_json::from_str(&instances)?;
        instances[index.to_string()] = json!({"status":status,"outputs":outputs,"error":error,"run":id.run,"inputs":frozen["inputs"]});
        let done = instances
            .as_object()
            .expect("instances object")
            .values()
            .filter(|v| v["status"] == "succeeded" || v["status"] == "failed")
            .count() as u64;
        if done < total as u64 {
            step_status = StepStatus::Running;
        } else {
            let failed = instances
                .as_object()
                .expect("instances object")
                .values()
                .find(|v| v["status"] == "failed");
            step_status = if failed.is_some() {
                StepStatus::Failed
            } else {
                StepStatus::Succeeded
            };
            step_error = failed
                .map(|v| serde_json::from_value(v["error"].clone()))
                .transpose()?;
            step_outputs = JsonMap::default();
            for name in schema
                .as_object()
                .ok_or_else(|| plans::invalid("frozen schema is not an object"))?
                .keys()
            {
                let values: Vec<Value> = (0..total)
                    .map(|i| instances[i.to_string()]["outputs"][name].clone())
                    .collect();
                step_outputs
                    .0
                    .insert(name.clone(), Value::Array(values).try_into()?);
            }
        }
        tx.sql().execute(
            "UPDATE steps SET instances=?3,done=?4 WHERE project_id=?1 AND step_id=?2",
            params![
                id.project.to_string(),
                id.step.as_str(),
                instances.to_string(),
                plans::sql_counter(done)?
            ],
        )?;
    }
    let mut result = None;
    if step_status != StepStatus::Running {
        tx.sql().execute("UPDATE steps SET status=?3,outputs=?4,error=?5,manual=0,result_id=NULL WHERE project_id=?1 AND step_id=?2",params![id.project.to_string(),id.step.as_str(),step_status.as_str(),serde_json::to_string(&step_outputs)?,step_error.as_ref().map(serde_json::to_string).transpose()?])?;
        let effective: JsonMap = serde_json::from_value(frozen["effective_inputs"].clone())?;
        result = Some(plans::snapshot_result(
            tx,
            id.project,
            &id.step,
            Some(id.attempt),
            Some(&effective),
        )?);
        plans::status_record(
            tx,
            id.project,
            &id.step,
            state.status(&id.step),
            step_status,
            step_error,
        )?;
    }
    if result.is_none() {
        plans::status_record(
            tx,
            id.project,
            &id.step,
            StepStatus::Running,
            StepStatus::Running,
            None,
        )?;
    }
    hooks.release(tx, id)?;
    let action = if let Some(registered) = registered {
        let registered: Value = serde_json::from_str(&registered)?;
        let target: CompletionActionTarget = serde_json::from_value(registered["target"].clone())?;
        let author = registered["author"]
            .as_str()
            .ok_or_else(|| plans::invalid("action author missing"))?;
        let body = registered["message"]
            .as_str()
            .ok_or_else(|| plans::invalid("action message missing"))?;
        let outcome = if rejected {
            match completion.current {
                Ok(current) => apply_action(tx, current, &target, body, author, hooks)?,
                Err(message) => CompletionActionOutcome::Conflict(CompletionActionConflict {
                    current: completion_target(tx, id.project, &target.step)?,
                    expected: target,
                    message: format!("current plan reconciliation failed: {message}"),
                }),
            }
        } else {
            CompletionActionOutcome::Discarded
        };
        tx.append_record(
            Some(id.project),
            Event::RunCompletionAction {
                run: id.run,
                outcome: outcome.clone(),
                author: author.into(),
            },
        )?;
        Some(outcome)
    } else {
        None
    };
    if let Ok(current) = completion.current {
        plans::reconcile(tx, current)?;
    }
    let outcome = CompletionResult {
        status,
        outputs,
        error,
        result,
        action,
    };
    tx.sql().execute(
        "UPDATE runs SET result=?2,action_outcome=?3 WHERE run_id=?1",
        params![
            id.run.to_string(),
            completion_json(&outcome).to_string(),
            outcome
                .action
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?
        ],
    )?;
    tx.changed(Some(id.project), "status");
    Ok(Some(outcome))
}
fn apply_action(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    target: &CompletionActionTarget,
    body: &str,
    author: &str,
    hooks: &mut impl ExecutionHooks,
) -> Result<CompletionActionOutcome> {
    let current = completion_target(tx, context.project, &target.step)?;
    let state = plans::read_state(tx.sql(), context.project)?;
    if current.as_ref() != Some(target)
        || !matches!(
            state.status(&target.step),
            StepStatus::Succeeded | StepStatus::Failed | StepStatus::Stale
        )
    {
        return Ok(CompletionActionOutcome::Conflict(
            CompletionActionConflict {
                expected: target.clone(),
                current,
                message: "completion action target changed or is not retryable".into(),
            },
        ));
    }
    let walk = match plans::prepare_retry(
        tx,
        context,
        std::slice::from_ref(&target.step),
        Some(body),
        author,
        hooks,
    ) {
        Ok(walk) => walk,
        Err(StoreError::Public(error)) => {
            return Ok(CompletionActionOutcome::Conflict(
                CompletionActionConflict {
                    expected: target.clone(),
                    current,
                    message: error.to_string(),
                },
            ));
        }
        Err(error) => return Err(error),
    };
    Ok(CompletionActionOutcome::Applied(plans::apply_retry(
        tx,
        context,
        walk,
        Some(body),
        author,
        "conditional Rejected retry",
        hooks,
    )?))
}

pub fn lost(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    identity: AttemptIdentity,
    completion_id: String,
    processes_gone: bool,
    hooks: &mut impl ExecutionHooks,
) -> Result<Option<CompletionResult>> {
    let version: Option<i64> = tx
        .sql()
        .query_row(
            "SELECT version FROM submissions WHERE run_id=?1",
            [identity.run.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let result = complete(
        tx,
        context,
        Complete {
            identity,
            completion_id,
            kind: CompletionKind::Lost {
                message: "invocation process lost".into(),
            },
            outputs: JsonMap::default(),
            processes_gone,
            submission_version: version.map(|v| v as u64),
        },
        hooks,
    )?;
    Ok(result)
}

/// Each running step of the project whose cancel was asked for (`step_cancel`) and whose run
/// has not ended: it is stopping. A settle also asks its run to stop, but settles it on its
/// submission, so it is not one.
pub fn stopping(
    sql: &rusqlite::Connection,
    project: ProjectId,
) -> Result<std::collections::BTreeSet<StepId>> {
    let mut q = sql.prepare_cached(
        "SELECT DISTINCT s.step_id FROM steps s JOIN attempts a ON a.project_id=s.project_id AND a.step_id=s.step_id AND a.generation=s.generation
         WHERE s.project_id=?1 AND s.status='running' AND a.phase<>'terminal' AND a.cancel_requested AND json_type(a.request,'$.settle') IS NULL",
    )?;
    let rows = q
        .query_map([project.to_string()], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|step| {
            step.parse()
                .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))
        })
        .collect()
}

/// Each running step of the project whose current runs have all stored a valid submission
/// (SPEC §6.4): its agent's work is done and it is only finishing. `since` is the latest
/// submission, `release` the run that made it.
pub fn finishing(
    sql: &rusqlite::Connection,
    project: ProjectId,
) -> Result<std::collections::BTreeMap<StepId, Finishing>> {
    let mut q = sql.prepare_cached(
        "SELECT s.step_id,r.release_id,m.at,(SELECT max(seq) FROM records WHERE run_id=r.run_id AND kind='step.submit')
         FROM steps s JOIN json_each(s.run_ids) j JOIN runs r ON r.run_id=j.value LEFT JOIN submissions m ON m.run_id=r.run_id
         WHERE s.project_id=?1 AND s.status='running' ORDER BY s.step_id,m.at",
    )?;
    let rows = q
        .query_map([project.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut out: std::collections::BTreeMap<StepId, Option<Finishing>> = Default::default();
    for (step, release, at, seq) in rows {
        let step: StepId = step
            .parse()
            .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?;
        let entry = out.entry(step).or_insert_with(|| {
            Some(Finishing {
                since: String::new(),
                submission_seq: None,
                release: String::new(),
            })
        });
        match (entry.as_mut(), at) {
            (Some(finishing), Some(at)) => {
                finishing.since = at;
                finishing.submission_seq = seq;
                finishing.release = short_release(release.as_deref().unwrap_or_default());
            }
            _ => *entry = None,
        }
    }
    Ok(out
        .into_iter()
        .filter_map(|(step, finishing)| finishing.map(|f| (step, f)))
        .collect())
}

/// A call's run that has submitted and is not finished: the call is only finishing.
pub fn call_finishing(sql: &rusqlite::Connection, call: RunId) -> Result<Option<Finishing>> {
    Ok(sql
        .query_row(
            "SELECT r.release_id,m.at,(SELECT max(seq) FROM records WHERE run_id=r.run_id AND kind='step.submit')
             FROM runs r JOIN submissions m ON m.run_id=r.run_id WHERE r.run_id=?1 AND r.finished_at IS NULL",
            [call.to_string()],
            |r| {
                Ok(Finishing {
                    release: short_release(r.get::<_, Option<String>>(0)?.as_deref().unwrap_or_default()),
                    since: r.get(1)?,
                    submission_seq: r.get(2)?,
                })
            },
        )
        .optional()?)
}

/// Which attempt at `step` this is and how the one before it ended (SPEC §7.4): for
/// `current`, the run its `prev_run` names; without it, the latest finished run of the step's
/// generation, which its next attempt would follow. The work tree is the caller's: it needs
/// git.
pub fn attempt_note(
    sql: &rusqlite::Connection,
    project: ProjectId,
    step: &StepId,
    current: Option<RunId>,
) -> Result<AttemptNote> {
    let (previous, attempt) = match current {
        Some(run) => {
            let row: Option<(Option<String>, i64, i64, String)> = sql
                .query_row(
                    "SELECT prev_run,generation,item_index,created_at FROM runs WHERE run_id=?1 AND project_id=?2 AND step_id=?3",
                    params![run.to_string(), project.to_string(), step.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((prev, generation, item, created)) = row else {
                return Ok(AttemptNote {
                    number: 1,
                    previous: None,
                    worktree: None,
                });
            };
            let earlier: i64 = sql.query_row(
                "SELECT count(*) FROM runs WHERE project_id=?1 AND step_id=?2 AND generation=?3 AND item_index=?4
                 AND (created_at<?5 OR (created_at=?5 AND run_id<?6))",
                params![project.to_string(), step.as_str(), generation, item, created, run.to_string()],
                |r| r.get(0),
            )?;
            (prev, earlier + 1)
        }
        None => {
            let generation: Option<i64> = sql
                .query_row(
                    "SELECT generation FROM steps WHERE project_id=?1 AND step_id=?2",
                    params![project.to_string(), step.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(generation) = generation else {
                return Ok(AttemptNote {
                    number: 1,
                    previous: None,
                    worktree: None,
                });
            };
            let latest: Option<String> = sql
                .query_row(
                    "SELECT r.run_id FROM runs r JOIN attempts a USING(attempt_id)
                     WHERE r.project_id=?1 AND r.step_id=?2 AND r.generation=?3 AND r.item_index=-1 AND a.phase='terminal'
                     ORDER BY r.created_at DESC,r.run_id DESC LIMIT 1",
                    params![project.to_string(), step.as_str(), generation],
                    |r| r.get(0),
                )
                .optional()?;
            let count: i64 = sql.query_row(
                "SELECT count(*) FROM runs WHERE project_id=?1 AND step_id=?2 AND generation=?3 AND item_index=-1",
                params![project.to_string(), step.as_str(), generation],
                |r| r.get(0),
            )?;
            (latest, count + 1)
        }
    };
    let previous = previous
        .map(|run| previous_attempt(sql, project, step, &run))
        .transpose()?
        .flatten();
    Ok(AttemptNote {
        number: attempt as u64,
        previous,
        worktree: None,
    })
}

fn previous_attempt(
    sql: &rusqlite::Connection,
    project: ProjectId,
    step: &StepId,
    run: &str,
) -> Result<Option<PreviousAttempt>> {
    type Row = (
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<Row> = sql
        .query_row(
            "SELECT r.started_at,r.created_at,r.finished_at,r.result,json_extract(a.request,'$.settle')
             FROM runs r JOIN attempts a USING(attempt_id) WHERE r.run_id=?1 AND r.project_id=?2",
            params![run, project.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((started, created, finished, result, settle)) = row else {
        return Ok(None);
    };
    let result: Option<Value> = result.map(|r| serde_json::from_str(&r)).transpose()?;
    let settle: Option<Value> = settle.map(|s| serde_json::from_str(&s)).transpose()?;
    let error = result
        .as_ref()
        .map(|r| r["error"].clone())
        .filter(|e| e.is_object());
    let kind = error
        .as_ref()
        .and_then(|e| e["error"].as_str())
        .unwrap_or_default();
    let status = result
        .as_ref()
        .and_then(|r| r["status"].as_str())
        .unwrap_or_default();
    let ended = match (status, kind) {
        ("succeeded", _) if settle.is_some() => Ended::Settled,
        ("succeeded", _) => Ended::Succeeded,
        (_, "cancelled") => Ended::Cancelled,
        (_, "process_lost") => Ended::Lost,
        ("failed", _) => Ended::Failed,
        _ => Ended::Unknown,
    };
    let (mut by, mut reason) = (None, None);
    if let Some(settle) = &settle
        && ended == Ended::Settled
    {
        by = settle["author"].as_str().map(str::to_owned);
        reason = settle["reason"].as_str().map(str::to_owned);
    }
    if ended == Ended::Cancelled {
        // The log may have trimmed it; then who cancelled is unknown.
        let cancel: Option<String> = sql
            .query_row(
                "SELECT payload FROM records WHERE project_id=?1 AND step_id=?2 AND kind='step.cancel'
                 AND julianday(at)>=julianday(?3) AND julianday(at)<=julianday(coalesce(?4,'now')) ORDER BY seq DESC LIMIT 1",
                params![project.to_string(), step.as_str(), created, finished],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(cancel) = cancel {
            let cancel: Value = serde_json::from_str(&cancel)?;
            by = cancel["author"]
                .as_str()
                .filter(|a| !a.is_empty())
                .map(str::to_owned);
            reason = cancel["reason"].as_str().map(str::to_owned);
        }
    }
    let submitted: Option<String> = sql
        .query_row(
            "SELECT outputs FROM submissions WHERE run_id=?1",
            [run],
            |r| r.get(0),
        )
        .optional()?;
    let submitted: Option<Value> = submitted.map(|s| serde_json::from_str(&s)).transpose()?;
    let outputs = result
        .as_ref()
        .and_then(|r| r["outputs"].as_object().cloned())
        .unwrap_or_default();
    let git = outputs.get("git").and_then(GitSeen::from_output);
    let shown: serde_json::Map<String, Value> = outputs
        .into_iter()
        .filter(|(name, value)| name != "git" && !value.is_null())
        .collect();
    Ok(Some(PreviousAttempt {
        run: run.to_owned(),
        started,
        finished,
        ended,
        error: error.map(|e| cut_strings(&e, VALUE_CUT)),
        by,
        reason,
        outputs: (submitted.is_none() && !shown.is_empty())
            .then(|| cut_strings(&Value::Object(shown), VALUE_CUT)),
        submitted: submitted.map(|s| cut_strings(&s, VALUE_CUT)),
        git,
    }))
}

/// `step_settle` (SPEC §7.5): the step's one current run is finishing on a valid submission;
/// record the outputs the done signal would have given it and ask its guardian to stop the
/// payload, as a cancel does. Its completion then settles the step with them (see
/// `complete_frozen`). `outputs` were derived and checked against the frozen schema by the
/// caller. Refused (`conflict`) when the run moved on or is no longer finishing.
pub fn settle(
    tx: &mut WriteTransaction<'_>,
    id: &AttemptIdentity,
    outputs: &JsonMap,
    author: String,
    reason: String,
) -> Result<()> {
    if current_callback(tx, id)?.is_none_or(|phase| phase == "terminal") {
        return Err(plans::conflict(format!(
            "run {} of step {} is no longer the step's current run",
            id.run, id.step
        )));
    }
    let (cancel, settled, submitted, runs): (bool, bool, bool, String) = tx.sql().query_row(
        "SELECT a.cancel_requested,json_type(a.request,'$.settle') IS NOT NULL,EXISTS(SELECT 1 FROM submissions WHERE run_id=?2),
         (SELECT run_ids FROM steps WHERE project_id=?3 AND step_id=?4)
         FROM attempts a WHERE a.attempt_id=?1",
        params![id.attempt.to_string(), id.run.to_string(), id.project.to_string(), id.step.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if settled {
        return Err(plans::conflict(format!(
            "step {} is already being settled on its submission",
            id.step
        )));
    }
    if cancel {
        return Err(plans::conflict(format!(
            "step {} is being cancelled; retry it once it has failed",
            id.step
        )));
    }
    if !submitted || serde_json::from_str::<Vec<RunId>>(&runs)? != [id.run] {
        return Err(plans::conflict(format!(
            "step {} is no longer finishing on run {}",
            id.step, id.run
        )));
    }
    let at = plans::now()?;
    tx.sql().execute(
        "UPDATE attempts SET cancel_requested=1,request=json_set(request,'$.settle',json(?2)) WHERE attempt_id=?1",
        params![
            id.attempt.to_string(),
            json!({"outputs":outputs,"author":author,"reason":reason,"at":at}).to_string()
        ],
    )?;
    tx.append_record(
        Some(id.project),
        Event::StepSettle {
            step: id.step.clone(),
            run: id.run,
            author,
            reason,
        },
    )?;
    tx.changed(Some(id.project), "status");
    Ok(())
}

/// The identity of `step`'s one current, unfinished run and its frozen request, for
/// `step_settle`; None when the step does not have exactly one.
pub fn current_run(
    sql: &rusqlite::Connection,
    project: ProjectId,
    step: &StepId,
) -> Result<Option<(AttemptIdentity, Value)>> {
    let row: Option<(String, String, i64, i64, String)> = sql
        .query_row(
            "SELECT r.run_id,r.attempt_id,r.generation,r.work_generation,a.request
             FROM steps s JOIN runs r ON r.project_id=s.project_id AND r.step_id=s.step_id AND r.generation=s.generation
               AND r.work_generation=s.work_generation AND r.run_id=json_extract(s.run_ids,'$[0]')
             JOIN attempts a ON a.attempt_id=r.attempt_id
             WHERE s.project_id=?1 AND s.step_id=?2 AND s.status='running' AND json_array_length(s.run_ids)=1
               AND r.finished_at IS NULL AND a.phase<>'terminal'",
            params![project.to_string(), step.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((run, attempt, generation, work, request)) = row else {
        return Ok(None);
    };
    let parse = |e: String| StoreError::InvalidDatabase(e);
    Ok(Some((
        identity_from_row(
            project,
            step.clone(),
            attempt.parse().map_err(|e| parse(format!("{e}")))?,
            run.parse().map_err(|e| parse(format!("{e}")))?,
            generation,
            work,
        ),
        serde_json::from_str(&request)?,
    )))
}

/// Check `outputs` against a run's frozen result schema (its fn's outputs and the step's
/// declared ones), as its completion will.
pub fn check_frozen_outputs(frozen: &Value, outputs: &JsonMap) -> Result<()> {
    let mut schema = frozen["returns"].clone();
    if let (Some(schema), Some(declared)) = (schema.as_object_mut(), frozen["declared"].as_object())
    {
        schema.extend(declared.clone());
    }
    check_schema(&schema, outputs)
}
