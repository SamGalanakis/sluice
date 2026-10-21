//! Admission decisions use the model and durable resource accounting.
use crate::{
    coordinator::Coordinator,
    dispatch::{Hooks, new_capability},
    execution::{ExecutionHost, Launch, LaunchOutcome},
};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use sluice_model::{
    commands::{StepSetOutput, StepStatus},
    error::PublicError,
    events::Event,
    gates::{GateDecision, evaluate_step},
    hash::EffectiveInput,
    ids::*,
    plan::{effective_inputs, inputs_hash},
    rpc::{FnInvocation, JsonMap},
};
use sluice_process::{
    journal::{AttemptKey, CleanupEvidence, CompletionJournal, PayloadResult},
    socket::AssignedRange,
};
use sluice_store::{
    RetrySafety,
    attempts::{self, Reserve},
    plans, resources,
};
use std::{collections::BTreeSet, time::Duration};
use tokio_util::sync::CancellationToken;

pub const CAPACITY_INTERVAL: Duration = Duration::from_secs(10);
pub const FULL_INTERVAL: Duration = Duration::from_secs(30);
/// How often live runs' cgroup limit counters are read.
pub const LIMIT_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const RECONCILE_TRIES: usize = 3;

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod tests;

/// Recheck the connection-owned lease in the same transaction as every reservation.
fn lease(tx: &sluice_store::WriteTransaction<'_>, owner: &str) -> sluice_store::Result<()> {
    let actual: Option<String> = tx.sql().query_row(
        "SELECT scheduler_owner FROM maintenance WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if actual.as_deref() != Some(owner) {
        return Err(PublicError::Conflict {
            message: "scheduler lease lost".into(),
            current_rev: None,
        }
        .into());
    }
    Ok(())
}

pub async fn reconcile_project<H: ExecutionHost>(
    broker: &Coordinator<H>,
    project: ProjectId,
    owner: &str,
) -> Result<usize, PublicError> {
    reconcile_project_with(
        broker,
        project,
        owner,
        #[cfg(test)]
        async |_| {},
    )
    .await
}

async fn reconcile_project_with<H: ExecutionHost>(
    broker: &Coordinator<H>,
    project: ProjectId,
    owner: &str,
    #[cfg(test)] mut after_reconcile: impl AsyncFnMut(plans::PlanContext),
) -> Result<usize, PublicError> {
    let mut admitted = 0;
    let mut attempt = 1;
    loop {
        let result = reconcile_pass(
            broker,
            project,
            owner,
            &mut admitted,
            #[cfg(test)]
            &mut after_reconcile,
        )
        .await;
        match result {
            Err(PublicError::Conflict {
                ref message,
                current_rev,
            }) if attempt < RECONCILE_TRIES
                && (current_rev.is_some() || message == "compiled plan is stale") =>
            {
                attempt += 1
            }
            result => return result.map(|()| admitted),
        }
    }
}

async fn reconcile_pass<H: ExecutionHost>(
    broker: &Coordinator<H>,
    project: ProjectId,
    owner: &str,
    admitted: &mut usize,
    #[cfg(test)] after_reconcile: &mut impl AsyncFnMut(plans::PlanContext),
) -> Result<(), PublicError> {
    loop {
        let (policy, headroom) = crate::budget::execution(broker.home())?;
        let host = broker.host();
        let reading = crate::headroom::Reading::take(
            broker.reads(),
            host.host_reading(broker.home_id()).await,
            host.now(),
            |run, service| async move { host.run_memory(run, &service).await },
        )
        .await;
        let gate = crate::headroom::Gate::new(reading, broker.home(), headroom);
        if let Some(gate) = &gate {
            gate.record_peaks(broker.writer()).await?;
        }
        let context = broker.context(project).await?;
        let copy = context.clone();
        let lease_owner = owner.to_owned();
        let now = broker
            .host()
            .now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        broker
            .writer()
            .write(RetrySafety::Idempotent, move |tx| {
                lease(tx, &lease_owner)?;
                sluice_store::restart::reconcile(tx, &copy, now, &mut Hooks)?;
                plans::reconcile(tx, &copy)?;
                crate::watch::record_settlements(tx, project, copy.revision, &copy.plan)?;
                resources::grant_leases(tx, project)?;
                Ok(())
            })
            .await?;
        #[cfg(test)]
        after_reconcile(context.clone()).await;
        let copy = context.clone();
        let (state, order) = broker
            .reads()
            .snapshot(move |sql| {
                let state = plans::read_state(sql, project)?;
                let mut order: Vec<_> = copy
                    .plan
                    .topological_order()
                    .iter()
                    .filter(|id| copy.plan.steps()[*id].needs.is_empty())
                    .cloned()
                    .collect();
                order.extend(
                    resources::admit_order(sql, project, copy.revision, &copy.plan)?
                        .into_iter()
                        .map(|a| a.step),
                );
                for id in copy.plan.topological_order() {
                    if state.status(id) == StepStatus::Running
                        && copy.plan.steps()[id].scatter.is_some()
                        && !order.contains(id)
                    {
                        order.push(id.clone());
                    }
                }
                Ok((state, order))
            })
            .await
            .map_err(|e| e.into_public(true))?;
        let mut progressed = false;
        for id in order {
            let step = &context.plan.steps()[&id];
            let continuing = state.status(&id) == StepStatus::Running && step.scatter.is_some();
            if step.is_external()
                || if continuing {
                    state.paused.is_paused() || step.paused.is_paused()
                } else {
                    state.status(&id) != StepStatus::Pending
                        || !matches!(
                            evaluate_step(&context.plan, &state, step),
                            GateDecision::Ready
                        )
                }
            {
                continue;
            }
            let (hash, frozen) = if continuing {
                let batch = continued_batch(broker, project, id.clone()).await?;
                (
                    batch.hash,
                    Ok((batch.inputs, batch.fingerprints, Some(batch.count))),
                )
            } else {
                let Some(effective) = effective_inputs(&context.plan, &state, step) else {
                    continue;
                };
                let hash = inputs_hash(&context.plan, &state, step).ok_or_else(|| {
                    PublicError::Storage {
                        message: "ready inputs lack hash".into(),
                    }
                })?;
                let frozen = freeze_inputs(effective).and_then(|(inputs, fingerprints)| {
                    let count = step
                        .scatter
                        .as_ref()
                        .map(|name| {
                            inputs.0[name]
                                .as_value()
                                .as_array()
                                .map(Vec::len)
                                .ok_or_else(|| PublicError::FnFailure {
                                    message: "scatter must be an array".into(),
                                })
                        })
                        .transpose()?;
                    Ok((inputs, fingerprints, count))
                });
                (hash, frozen)
            };
            let needs = step.needs.iter().map(|(n, a)| (n.clone(), *a)).collect();
            if !continuing
                && !broker
                    .reads()
                    .snapshot(move |sql| resources::fits(sql, project, &needs))
                    .await
                    .map_err(|e| e.into_public(true))?
                    .fits()
            {
                continue;
            }
            let (inputs, fingerprints, count) = match frozen {
                Ok(frozen) => frozen,
                Err(error) => {
                    fail_ready(broker, context.clone(), id.clone(), hash, owner, error).await?;
                    progressed = true;
                    break;
                }
            };
            if count == Some(0) || step.run.starts_with("core.") {
                let outputs = if count == Some(0) {
                    let mut outputs = JsonMap::default();
                    for name in step
                        .signature
                        .outputs
                        .keys()
                        .chain(step.declared_outputs.keys())
                    {
                        outputs.0.insert(name.clone(), json!([]).try_into()?);
                    }
                    outputs
                } else {
                    let mut outputs = JsonMap::default();
                    let mut failure = None;
                    for index in 0..count.unwrap_or(1) {
                        let mut item = inputs.clone();
                        if let Some(name) = &step.scatter {
                            item.0.insert(
                                name.clone(),
                                inputs.0[name].as_value().as_array().expect("scatter array")[index]
                                    .clone()
                                    .try_into()?,
                            );
                        }
                        match broker
                            .host()
                            .invoke(FnInvocation {
                                project,
                                step: Some(id.clone()),
                                run: RunId::new(),
                                attempt: AttemptId::new(),
                                invocation: InvocationId::new(),
                                name: step.run.clone(),
                                inputs: item,
                            })
                            .await
                        {
                            Ok(result) => {
                                for (name, value) in result.0 {
                                    if count.is_some() {
                                        let entry =
                                            outputs.0.entry(name).or_insert(json!([]).try_into()?);
                                        let mut array = entry
                                            .as_value()
                                            .as_array()
                                            .expect("aggregate array")
                                            .clone();
                                        array.push(value.into_value());
                                        *entry = Value::Array(array).try_into()?;
                                    } else {
                                        outputs.0.insert(name, value);
                                    }
                                }
                            }
                            Err(e) => {
                                failure = Some(e);
                                break;
                            }
                        }
                    }
                    if let Some(error) = failure {
                        fail_ready(broker, context.clone(), id.clone(), hash, owner, error).await?;
                        progressed = true;
                        break;
                    }
                    outputs
                };
                let copy = context.clone();
                let owner = owner.to_string();
                let id = id.clone();
                crate::install::admission_write(
                    broker.writer(),
                    RetrySafety::NonIdempotent,
                    move |tx| {
                        lease(tx, &owner)?;
                        crate::drain::ensure_admission(tx, &crate::drain::Admission::Plan)?;
                        let needs = copy.plan.steps()[&id]
                            .needs
                            .iter()
                            .map(|(n, a)| (n.clone(), *a))
                            .collect();
                        if !resources::fits(tx.sql(), project, &needs)?.fits() {
                            return Err(PublicError::Conflict {
                                message: "inline resource availability changed".into(),
                                current_rev: None,
                            }
                            .into());
                        }
                        plans::settle_inline(
                            tx,
                            &copy,
                            hash,
                            StepSetOutput {
                                project: ProjectSelector::Id(project),
                                step: id,
                                outputs,
                                force: false,
                                reason: "inline completion".into(),
                                author: Some("scheduler".into()),
                            },
                        )?;
                        Ok(())
                    },
                )
                .await?;
                progressed = true;
                *admitted += 1;
                break;
            }
            let execution = broker
                .catalog()
                .1
                .as_ref()
                .map(|p| p.resolved(Some(project), &step.run))
                .transpose()?;
            let project_name = broker
                .reads()
                .snapshot(move |sql| {
                    Ok(
                        sluice_store::projects::resolve(sql, &ProjectSelector::Id(project))?
                            .name
                            .to_string(),
                    )
                })
                .await
                .map_err(|e| e.into_public(true))?;
            let copy = context.clone();
            let step = step.clone();
            let owner = owner.to_string();
            let home = broker.home_id();
            let policy = policy.clone();
            let gate = gate.clone();
            // A new batch rereads file fingerprints immediately before enqueue.
            // Continued scatter uses the original frozen contents even after a file changes.
            for entry in fingerprints.0.values().filter(|_| !continuing) {
                let entry = entry.as_value();
                let path = entry["path"].as_str().ok_or_else(|| PublicError::Storage {
                    message: "file provenance path".into(),
                })?;
                let fresh = std::fs::read(path).map_err(|e| PublicError::FnFailure {
                    message: e.to_string(),
                })?;
                if entry["fingerprint"] != sluice_store::artifacts::fingerprint(&fresh) {
                    return Err(PublicError::Conflict {
                        message: "bound file changed during admission".into(),
                        current_rev: None,
                    });
                }
            }
            let launches=crate::install::admission_write(broker.writer(),RetrySafety::NonIdempotent,move|tx|{
                lease(tx,&owner)?;
                resources::grant_leases(tx,project)?;
                if !continuing && !resources::fits(tx.sql(),project,&step.needs.iter().map(|(n,a)|(n.clone(),*a)).collect())?.fits(){return Ok(vec![]);}
                let limits = match crate::budget::resolve(tx.sql(), Some(project), step.limits.as_ref(), &policy) {
                    Ok(limits) => limits,
                    Err(sluice_store::StoreError::Public(PublicError::Invalid { .. })) => return Ok(vec![]),
                    Err(error) => return Err(error),
                };
                let (old_hash,total,instances):(Option<String>,Option<i64>,String)=tx.sql().query_row("SELECT inputs_hash,total,instances FROM steps WHERE project_id=?1 AND step_id=?2",(project.to_string(),step.id.as_str()),|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
                let instances:Value=serde_json::from_str(&instances)?;
                let keep=old_hash.as_deref()==Some(&hash.to_string()) && total==count.map(|n|n as i64);
                let mut launches=vec![];
                for index in 0..count.unwrap_or(1){
                    if count.is_some() && keep && instances.get(index.to_string()).is_some_and(|v|v["status"]=="succeeded" || continuing && matches!(v["status"].as_str(),Some("running"|"failed"))){continue;}
                    // The measured memory, counting the runs this batch already admitted at their charge.
                    if let Some(gate) = &gate && gate.hold(tx.sql(), &policy)?.is_some() { break; }
                    let mut inputs=inputs.clone();if let Some(name)=&step.scatter {inputs.0.insert(name.clone(),inputs.0[name].as_value().as_array().expect("checked scatter")[index].clone().try_into()?);}
                    let capability=new_capability();let attempt=AttemptId::new();let run=RunId::new();
                    let provenance:JsonMap=serde_json::from_value(json!({"runtime":{"capability":capability,"execution":execution,"admitted_rev":copy.revision},"files":fingerprints}))?;
                    let reservation=attempts::reserve(tx,&copy,Reserve{step:step.id.clone(),attempt,run,item_index:if count.is_some(){index as i64}else{-1},item_count:count.map(|n|n as u64),inputs:inputs.clone(),inputs_hash:hash,provenance,release_id:crate::install::release_id("runtime-v1"),protocol_major:1,limits,admission:policy.clone()},&mut Hooks)?;
                    if let Some(job) = execution.as_ref().and_then(|e| e.job) { crate::registry::pin_run(tx,job,run)?; }
                    if let Some(gate) = &gate { gate.admitted(tx.sql(), reservation.identity.attempt)?; }
                    let id=&reservation.identity;
                    if attempts::spawn_attempted(tx,id)? {
                        launches.push(Launch{identity:AttemptKey{home,project:Some(project),step:Some(id.step.clone()),generation:id.generation,work:id.work,run,attempt},invocation:FnInvocation{project,step:Some(id.step.clone()),run,attempt,invocation:InvocationId::new(),name:step.run.clone(),inputs},assigned:AssignedRange{after:MessageId(reservation.messages.after),through:MessageId(reservation.messages.through)},prev_run:reservation.prev_run,capability,limits,policy:policy.clone(),timeout_seconds:None,context:execution.clone().map(|execution| crate::compose::ReservationContext { execution, project_name:project_name.clone(), extra_inputs:JsonMap(step.extra_inputs.iter().map(|(n,t)| (n.clone(),json!({"type":t.form()}).try_into().expect("input declaration"))).collect()), outputs:JsonMap(step.declared_outputs.iter().map(|(n,d)| (n.clone(),json!({"type":d.ty.form(),"doc":d.doc}).try_into().expect("output declaration"))).collect()) })});
                    }
                }
                Ok(launches)
            }).await?;
            for launch in launches {
                let identity = launch.identity.clone();
                let limits = launch.limits;
                let guardian_memory_bytes = launch.policy.guardian_memory_bytes;
                match broker
                    .host()
                    .launch(launch)
                    .await
                    .unwrap_or_else(|e| LaunchOutcome::Uncertain(e.to_string()))
                {
                    LaunchOutcome::Accepted => {}
                    LaunchOutcome::Uncertain(message) => {
                        tracing::warn!(run=%identity.run,%message,"guardian start requires adoption")
                    }
                    LaunchOutcome::Refused(error) => {
                        broker
                            .complete(CompletionJournal {
                                protocol: 1,
                                limits,
                                guardian_memory_bytes,
                                usage: Some(Default::default()),
                                identity: identity.clone(),
                                completion_id: format!("spawn-{}", identity.attempt),
                                result: PayloadResult::Failed(error),
                                starts: vec![],
                                exits: vec![],
                                cleanup: vec![CleanupEvidence {
                                    cgroup: format!(
                                        "/unstarted/{}",
                                        sluice_process::systemd::TransientService::for_launch(
                                            identity.run
                                        )
                                        .name()
                                    ),
                                    empty: true,
                                    escalated: false,
                                }],
                                submissions: JsonMap::default(),
                                submission_version: None,
                                delivery_acks: vec![],
                            })
                            .await?;
                    }
                }
                *admitted += 1;
                progressed = true;
            }
        }
        if !progressed {
            let held = record_queues(broker, context, owner, policy, gate).await?;
            broker.note_host_hold(project, held);
            return Ok(());
        }
    }
}

struct ContinuedBatch {
    inputs: JsonMap,
    fingerprints: JsonMap,
    count: usize,
    hash: sluice_model::hash::InputsHash,
}

async fn continued_batch<H: ExecutionHost>(
    broker: &Coordinator<H>,
    project: ProjectId,
    step: StepId,
) -> Result<ContinuedBatch, PublicError> {
    broker
        .reads()
        .snapshot(move |sql| {
            let (request, hash): (String, String) = sql.query_row(
                "SELECT a.request,a.inputs_hash FROM attempts a JOIN steps s
             ON s.project_id=a.project_id AND s.step_id=a.step_id
             AND s.generation=a.generation AND s.work_generation=a.work_generation
             WHERE s.project_id=?1 AND s.step_id=?2 ORDER BY a.created_at,a.attempt_id LIMIT 1",
                (project.to_string(), step.as_str()),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let request: Value = serde_json::from_str(&request)?;
            let bad = || {
                sluice_store::StoreError::InvalidDatabase(
                    "running scatter has an invalid frozen batch".into(),
                )
            };
            let name = request["declaration"]["scatter"].as_str().ok_or_else(bad)?;
            let array = request["effective_inputs"][name]
                .as_array()
                .ok_or_else(bad)?;
            let count = usize::try_from(request["item_count"].as_u64().ok_or_else(bad)?)
                .map_err(|_| bad())?;
            if array.len() != count {
                return Err(bad());
            }
            let mut inputs: JsonMap = serde_json::from_value(request["inputs"].clone())?;
            inputs.0.insert(
                name.into(),
                Value::Array(array.clone())
                    .try_into()
                    .map_err(sluice_store::StoreError::from)?,
            );
            let fingerprints = serde_json::from_value(request["provenance"]["files"].clone())?;
            let hash = hash.parse().map_err(|_| bad())?;
            Ok(ContinuedBatch {
                inputs,
                fingerprints,
                count,
                hash,
            })
        })
        .await
        .map_err(|error| error.into_public(true))
}

fn freeze_inputs(
    effective: indexmap::IndexMap<String, EffectiveInput>,
) -> Result<(JsonMap, JsonMap), PublicError> {
    let mut inputs = JsonMap::default();
    let mut fingerprints = JsonMap::default();
    for (name, value) in effective {
        let value = match value {
            EffectiveInput::Value(value) => value,
            EffectiveInput::File(path) => {
                let bytes = std::fs::read(&path).map_err(|e| PublicError::FnFailure {
                    message: format!("file input {path}: {e}"),
                })?;
                fingerprints.0.insert(
                    name.clone(),
                    json!({"path":path,"fingerprint":sluice_store::artifacts::fingerprint(&bytes)})
                        .try_into()?,
                );
                Value::String(
                    String::from_utf8(bytes).map_err(|e| PublicError::FnFailure {
                        message: e.to_string(),
                    })?,
                )
                .try_into()?
            }
        };
        inputs.0.insert(name, value);
    }
    Ok((inputs, fingerprints))
}

async fn fail_ready<H: ExecutionHost>(
    broker: &Coordinator<H>,
    context: plans::PlanContext,
    id: StepId,
    hash: sluice_model::hash::InputsHash,
    owner: &str,
    error: PublicError,
) -> Result<(), PublicError> {
    let owner = owner.to_owned();
    broker
        .writer()
        .write(RetrySafety::NonIdempotent, move |tx| {
            lease(tx, &owner)?;
            crate::drain::ensure_admission(tx, &crate::drain::Admission::Plan)?;
            plans::fail_unlaunched(tx, &context, &id, hash, error)
        })
        .await
}

/// Record why each ready step waits; returns whether any waits on the host's memory or CPU.
async fn record_queues<H: ExecutionHost>(
    broker: &Coordinator<H>,
    context: plans::PlanContext,
    owner: &str,
    policy: sluice_model::ResourcePolicy,
    gate: Option<crate::headroom::Gate>,
) -> Result<bool, PublicError> {
    let owner = owner.to_owned();
    broker.writer().write(RetrySafety::Idempotent, move |tx| {
        let mut host_held = false;
        lease(tx, &owner)?;
        let state = plans::read_state(tx.sql(), context.project)?;
        for step in context.plan.steps().values() {
            let continuing = state.status(&step.id) == StepStatus::Running && step.scatter.is_some();
            if step.is_external() || if continuing {
                state.paused.is_paused() || step.paused.is_paused()
            } else {
                state.status(&step.id) != StepStatus::Pending || !matches!(evaluate_step(&context.plan,&state,step),GateDecision::Ready)
            } { continue; }
            if continuing {
                let waiting: bool = tx.sql().query_row(
                    "SELECT total > (SELECT count(*) FROM json_each(instances)) FROM steps WHERE project_id=?1 AND step_id=?2",
                    (context.project.to_string(),step.id.as_str()), |row| row.get(0),
                )?;
                if !waiting { continue; }
            }
            let needs = step.needs.iter().map(|(n,a)|(n.clone(),*a)).collect();
            let mut fit = if continuing { resources::Fit { blocked: vec![], reason: String::new() } }
                else { resources::fits(tx.sql(), context.project, &needs)? };
            let mut host_hold = None;
            if fit.fits() {
                fit = match crate::budget::resolve(tx.sql(), Some(context.project), step.limits.as_ref(), &policy) {
                    Ok(_) => match &gate {
                        Some(gate) => match gate.hold(tx.sql(), &policy)? {
                            // The record keeps the hold's kind; the numbers change every tick
                            // and go to status and the owner's say instead.
                            Some(hold) => {
                                let reason = hold.reason();
                                let kind = reason.split(" (").next().unwrap_or(reason).to_owned();
                                let blocked = vec![hold.resource().into()];
                                host_hold = Some(hold);
                                resources::Fit { blocked, reason: kind }
                            }
                            None => fit,
                        },
                        None => fit,
                    },
                    Err(sluice_store::StoreError::Public(PublicError::Invalid { message, .. })) => resources::Fit { blocked: vec!["limits".into()], reason: message },
                    Err(error) => return Err(error),
                };
            }
            if fit.fits() { continue; }
            let event = Event::StepQueued {
                step: step.id.clone(),
                needs: serde_json::from_value(json!(needs))?,
                resources: JsonMap(fit.blocked.into_iter().map(|name| Ok((name, json!(true).try_into()?))).collect::<Result<_,PublicError>>()?),
                reason: fit.reason,
            };
            let previous: Option<String> = tx.sql().query_row("SELECT payload FROM records WHERE project_id=?1 AND step_id=?2 AND kind IN ('step.queued','step.status') ORDER BY seq DESC LIMIT 1", (context.project.to_string(), step.id.as_str()), |r| r.get(0)).optional()?;
            if previous.map(|s| serde_json::from_str::<Value>(&s)).transpose()?.as_ref() != Some(&serde_json::to_value(&event)?) {
                tx.append_record(Some(context.project), event)?;
            }
            // A step held for the host's memory or CPU is told to its owner once per hold.
            if let Some(hold) = host_hold {
                host_held = true;
                let since: String = tx.sql().query_row(
                    "SELECT min(at) FROM records WHERE project_id=?1 AND step_id=?2 AND kind='step.queued'
                     AND seq > coalesce((SELECT max(seq) FROM records WHERE project_id=?1 AND step_id=?2 AND kind='step.status'),0)",
                    (context.project.to_string(), step.id.as_str()), |r| r.get(0))?;
                let headroom = gate.as_ref().map(crate::headroom::Gate::headroom).copied();
                let notice = resources::LimitNotice {
                    run: None,
                    step: Some(step.id.clone()),
                    limit: hold.limit(),
                    value: match hold {
                        crate::headroom::Hold::Memory(_) => json!({"host_reserve_bytes": headroom.map(|h| h.host_reserve_bytes), "memory_pressure_max": headroom.map(|h| h.pressure_max), "admission_charge_bytes": headroom.map(|h| h.charge_bytes), "total_memory_bytes": policy.total.memory_bytes}),
                        crate::headroom::Hold::Cpu(_) => json!({"cpu_pressure_max": headroom.map(|h| h.cpu_pressure_max), "load_per_cpu_max": headroom.map(|h| h.load_per_cpu_max), "warmup_starts": headroom.map(|h| h.warmup_starts), "admission_warmup_s": headroom.map(|h| h.warmup.as_secs_f64())}),
                    },
                    observed: json!(hold.reason()),
                    outcome: "held",
                    since: Some(since),
                };
                let short = match hold {
                    crate::headroom::Hold::Memory(_) => "memory",
                    crate::headroom::Hold::Cpu(_) => "CPU",
                };
                let body = format!("Step {} is {}. Sluice checks again every {} s and starts it once the host has the room; nothing needs doing unless the host stays short of {short}.", step.id, hold.reason(), crate::headroom::RECHECK_INTERVAL.as_secs());
                resources::notify_limit(tx, context.project, &notice, body, false)?;
            }
        }
        Ok(host_held)
    }).await
}

fn registry_version<H: ExecutionHost>(broker: &Coordinator<H>) -> Option<u64> {
    broker.catalog().1.as_ref().map(|p| p.registry.version())
}

/// Refresh the published registry (which re-fingerprints every scope) and mark
/// every project for reconciliation only when that changed what a project sees.
async fn registry_changed<H: ExecutionHost>(
    broker: &Coordinator<H>,
    seen: &mut Option<u64>,
    dirty: &mut BTreeSet<ProjectId>,
) -> Result<(), PublicError> {
    if let Err(error) = broker.refresh_registry().await {
        tracing::warn!(%error, "registry refresh deferred");
    }
    let current = registry_version(broker);
    if current != *seen {
        *seen = current;
        dirty.extend(broker.projects().await?);
    }
    Ok(())
}

/// The full tick's upkeep, read from config.json each time: the "nobody reading" owner
/// question once `unread_alert_min` is set, then each log trimmed to 90% of `log_max` once it
/// holds more.
pub async fn upkeep(
    writer: &sluice_store::Writer,
    home: &std::path::Path,
    projects: Vec<ProjectId>,
) -> Result<(), PublicError> {
    let config = crate::config::HomeConfig::load(home);
    if let Some(minutes) = config.unread_alert_min {
        crate::watch::unread_alerts(writer, minutes).await?;
    }
    let (most, keep) = (config.log_max, config.log_keep());
    writer
        .write(RetrySafety::Idempotent, move |tx| {
            sluice_store::records::trim_to(tx, None, most, keep)?;
            for project in &projects {
                sluice_store::records::trim_to(tx, Some(*project), most, keep)?;
            }
            Ok(())
        })
        .await
}

/// Durable versions recover coalesced notifications; timers recover dropped wakes.
pub async fn run<H: ExecutionHost>(
    broker: Coordinator<H>,
    stop: CancellationToken,
) -> Result<(), PublicError> {
    let mut notify = broker.writer().subscribe();
    let mut watcher = broker
        .catalog()
        .1
        .as_ref()
        .map(|p| p.registry.watch())
        .transpose()?;
    let mut registry_tick = tokio::time::interval(Duration::from_secs(2));
    let mut registry_seen = registry_version(&broker);
    let mut versions = std::collections::BTreeMap::new();
    let mut full = tokio::time::interval(FULL_INTERVAL);
    let mut capacity = tokio::time::interval(CAPACITY_INTERVAL);
    let mut terminal_reaper = tokio::time::interval(Duration::from_millis(500));
    let mut limit_sampler = tokio::time::interval(LIMIT_SAMPLE_INTERVAL);
    let mut host_recheck = tokio::time::interval(crate::headroom::RECHECK_INTERVAL);
    let config_path = broker.home().join("config.json");
    let config_stamp =
        |path: &std::path::Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut config_seen = config_stamp(&config_path);
    let mut dirty = BTreeSet::new();
    let mut notifier = crate::notify::Notifier::new(
        broker.home().to_owned(),
        broker.writer().clone(),
        broker.reads().clone(),
    );
    // `serve` starts the scheduler right after its startup adoption pass, so
    // the immediate first full tick does not adopt again.
    let mut adopted_at_start = true;
    let mut retiring = crate::retire::Background::default();
    loop {
        // Owner notifications go out whether or not anything holds the scheduler lease.
        if let Err(error) = notifier.tick().await {
            tracing::warn!(%error, "owner notification deferred");
        }
        {
            let current = broker.project_versions().await?;
            for (project, version) in &current {
                if versions.get(project) != Some(version) {
                    dirty.insert(*project);
                }
            }
            versions = current;
        }
        if let Some(owner) = broker.scheduler_owner().await? {
            for project in std::mem::take(&mut dirty) {
                match reconcile_project(&broker, project, &owner).await {
                    Ok(_) => {}
                    Err(e) => tracing::warn!(%project,error=%e,"project reconciliation deferred"),
                }
            }
            for (call, project, _) in broker.calls().queued().await? {
                if let Err(e) = broker.calls().admit_queued(call, project).await {
                    tracing::warn!(%call,error=%e,"queued call deferred");
                }
            }
        }
        tokio::select! {
            _=stop.cancelled()=>return Ok(()),
            result=notify.changed()=>{if result.is_err(){return Ok(());}},
            _=async { match watcher.as_mut() { Some(w) => { let _ = w.changed().await; }, None => std::future::pending::<()>().await } }=>{ registry_changed(&broker, &mut registry_seen, &mut dirty).await?; },
            _=terminal_reaper.tick()=>{
                if let Err(error) = broker.reap_terminal(false).await { tracing::warn!(%error,"terminal guardian cleanup deferred"); }
            },
            _=registry_tick.tick()=>{
                if watcher.is_some() { registry_changed(&broker, &mut registry_seen, &mut dirty).await?; }
                // A changed execution policy applies at the next admission, without a drain.
                let stamp = config_stamp(&config_path);
                if stamp != config_seen {
                    config_seen = stamp;
                    dirty.extend(broker.projects().await?);
                }
            },
            // A run held for the host's memory or CPU waits on no write; check it again.
            _=host_recheck.tick()=>{ dirty.extend(broker.host_holds()); },
            _=limit_sampler.tick()=>{
                if broker.scheduler_owner().await?.is_some() && let Err(error) = broker.sample_limits().await {
                    tracing::warn!(%error,"execution limit sampling deferred");
                }
            },
            _=full.tick()=>{
                let projects = broker.projects().await?;
                dirty.extend(projects.iter().copied());
                if !std::mem::take(&mut adopted_at_start) {
                    broker.adopt().await?;
                }
                upkeep(broker.writer(), broker.home(), projects).await?;
                crate::calls::retain_calls(broker.writer()).await?;
                retiring.tick(&broker).await;
            },
            _=capacity.tick()=>{
                if broker.scheduler_owner().await?.is_some(){for project in broker.projects().await?{for resource in broker.capacity_resources(project).await?{if let Err(e)=broker.calls().capacity_call(project,resource).await{tracing::warn!(%project,error=%e,"capacity observation deferred");}}}}

            }
        }
    }
}
