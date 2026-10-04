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
    let mut admitted = 0;
    loop {
        let context = broker.context(project).await?;
        let copy = context.clone();
        let lease_owner = owner.to_owned();
        broker
            .writer()
            .write(RetrySafety::Idempotent, move |tx| {
                lease(tx, &lease_owner)?;
                plans::reconcile(tx, &copy)?;
                crate::watch::record_settlements(tx, project, &copy.plan)?;
                resources::grant_leases(tx, project)?;
                Ok(())
            })
            .await?;
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
                    resources::admit_order(sql, project, &copy.plan)?
                        .into_iter()
                        .map(|a| a.step),
                );
                Ok((state, order))
            })
            .await
            .map_err(|e| e.into_public(true))?;
        let mut progressed = false;
        for id in order {
            let step = &context.plan.steps()[&id];
            if state.status(&id) != StepStatus::Pending
                || !matches!(
                    evaluate_step(&context.plan, &state, step),
                    GateDecision::Ready
                )
                || step.is_external()
            {
                continue;
            }
            let Some(effective) = effective_inputs(&context.plan, &state, step) else {
                continue;
            };
            let hash =
                inputs_hash(&context.plan, &state, step).ok_or_else(|| PublicError::Storage {
                    message: "ready inputs lack hash".into(),
                })?;
            let needs = step.needs.iter().map(|(n, a)| (n.clone(), *a)).collect();
            if !broker
                .reads()
                .snapshot(move |sql| resources::fits(sql, project, &needs))
                .await
                .map_err(|e| e.into_public(true))?
                .fits()
            {
                continue;
            }
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
                admitted += 1;
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
            // Re-read file fingerprints immediately before enqueue. The transaction
            // independently rechecks the graph and effective bindings.
            for entry in fingerprints.0.values() {
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
                if !resources::fits(tx.sql(),project,&step.needs.iter().map(|(n,a)|(n.clone(),*a)).collect())?.fits(){return Ok(vec![]);}
                let (old_hash,total,instances):(Option<String>,Option<i64>,String)=tx.sql().query_row("SELECT inputs_hash,total,instances FROM steps WHERE project_id=?1 AND step_id=?2",(project.to_string(),step.id.as_str()),|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
                let instances:Value=serde_json::from_str(&instances)?;
                let keep=old_hash.as_deref()==Some(&hash.to_string()) && total==count.map(|n|n as i64);
                let mut launches=vec![];
                for index in 0..count.unwrap_or(1){
                    if count.is_some() && keep && instances.get(index.to_string()).is_some_and(|v|v["status"]=="succeeded"){continue;}
                    let mut inputs=inputs.clone();if let Some(name)=&step.scatter {inputs.0.insert(name.clone(),inputs.0[name].as_value().as_array().expect("checked scatter")[index].clone().try_into()?);}
                    let capability=new_capability();let attempt=AttemptId::new();let run=RunId::new();
                    let provenance:JsonMap=serde_json::from_value(json!({"runtime":{"capability":capability,"execution":execution,"completion":crate::execution::FrozenPlan::from_context(&copy)},"files":fingerprints}))?;
                    let reservation=attempts::reserve(tx,&copy,Reserve{step:step.id.clone(),attempt,run,item_index:if count.is_some(){index as i64}else{-1},item_count:count.map(|n|n as u64),inputs:inputs.clone(),inputs_hash:hash,provenance,release_id:crate::install::release_id("runtime-v1"),protocol_major:1},&mut Hooks)?;
                    if let Some(job) = execution.as_ref().and_then(|e| e.job) { crate::registry::pin_run(tx,job,run)?; }
                    let id=&reservation.identity;
                    if attempts::spawn_attempted(tx,id)? {
                        launches.push(Launch{identity:AttemptKey{home,project:Some(project),step:Some(id.step.clone()),generation:id.generation,work:id.work,run,attempt},invocation:FnInvocation{project,step:Some(id.step.clone()),run,attempt,invocation:InvocationId::new(),name:step.run.clone(),inputs},assigned:AssignedRange{after:MessageId(reservation.messages.after),through:MessageId(reservation.messages.through)},prev_run:reservation.prev_run,capability,timeout_seconds:None,context:execution.clone().map(|execution| crate::compose::ReservationContext { execution, project_name:project_name.clone(), extra_inputs:JsonMap(step.extra_inputs.iter().map(|(n,t)| (n.clone(),json!({"type":t.form()}).try_into().expect("input declaration"))).collect()), outputs:JsonMap(step.declared_outputs.iter().map(|(n,d)| (n.clone(),json!({"type":d.ty.form(),"doc":d.doc}).try_into().expect("output declaration"))).collect()) })});
                    }
                }
                Ok(launches)
            }).await?;
            for launch in launches {
                let identity = launch.identity.clone();
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
                admitted += 1;
                progressed = true;
            }
        }
        if !progressed {
            record_queues(broker, context, owner).await?;
            return Ok(admitted);
        }
    }
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

async fn record_queues<H: ExecutionHost>(
    broker: &Coordinator<H>,
    context: plans::PlanContext,
    owner: &str,
) -> Result<(), PublicError> {
    let owner = owner.to_owned();
    broker.writer().write(RetrySafety::Idempotent, move |tx| {
        lease(tx, &owner)?;
        for candidate in resources::admit_order(tx.sql(), context.project, &context.plan)? {
            let fit = resources::fits(tx.sql(), context.project, &candidate.needs)?;
            if fit.fits() { continue; }
            let event = Event::StepQueued {
                step: candidate.step.clone(),
                needs: serde_json::from_value(json!(candidate.needs))?,
                resources: JsonMap(fit.blocked.into_iter().map(|name| Ok((name, json!(true).try_into()?))).collect::<Result<_,PublicError>>()?),
                reason: fit.reason,
            };
            let previous: Option<String> = tx.sql().query_row("SELECT payload FROM records WHERE project_id=?1 AND step_id=?2 AND kind IN ('step.queued','step.status') ORDER BY seq DESC LIMIT 1", (context.project.to_string(), candidate.step.as_str()), |r| r.get(0)).optional()?;
            if previous.map(|s| serde_json::from_str::<Value>(&s)).transpose()?.as_ref() != Some(&serde_json::to_value(&event)?) {
                tx.append_record(Some(context.project), event)?;
            }
        }
        Ok(())
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
    let mut dirty = BTreeSet::new();
    let mut notifier = crate::notify::Notifier::new(
        broker.home().to_owned(),
        broker.writer().clone(),
        broker.reads().clone(),
    );
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
            _=registry_tick.tick()=>{ if watcher.is_some() { registry_changed(&broker, &mut registry_seen, &mut dirty).await?; } },
            _=full.tick()=>{
                let projects = broker.projects().await?;
                dirty.extend(projects.iter().copied());
                broker.adopt().await?;
                upkeep(broker.writer(), broker.home(), projects).await?;
                crate::calls::retain_calls(broker.writer()).await?;
            },
            _=capacity.tick()=>{
                if broker.scheduler_owner().await?.is_some(){for project in broker.projects().await?{for resource in broker.capacity_resources(project).await?{if let Err(e)=broker.calls().capacity_call(project,resource).await{tracing::warn!(%project,error=%e,"capacity observation deferred");}}}}

            }
        }
    }
}
