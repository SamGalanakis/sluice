//! Durable calls. Admission and guardian handoff belong to the coordinator, never the waiter.
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sluice_model::{
    commands::{FnCall, StepStatus},
    error::PublicError,
    events::Event,
    hash::InputsHash,
    ids::{AttemptId, ProjectId, RunId},
    plan::FnSignature,
    rpc::JsonMap,
    types::{Type, check_value_at},
};
use sluice_store::{
    ChangeKey, ReadPool, RetrySafety, StoreError, WriteTransaction, Writer, messages, resources,
};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::task::TaskTracker;

pub const MAX_WAIT_SECONDS: u64 = 3600;

/// The registry freezes the complete bundle outside the writer transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenFunction {
    pub name: String,
    pub inputs: IndexMap<String, Type>,
    pub outputs: IndexMap<String, Type>,
    pub bundle: JsonMap,
    pub needs: BTreeMap<String, u64>,
    pub release_id: String,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}
impl FrozenFunction {
    pub fn from_signature(name: String, signature: FnSignature, release_id: String) -> Self {
        Self {
            name,
            inputs: signature.inputs,
            outputs: signature.outputs,
            bundle: JsonMap::default(),
            needs: BTreeMap::new(),
            release_id,
            timeout_seconds: None,
        }
    }
}
pub trait CallRegistry: Send + Sync + 'static {
    fn freeze(&self, project: Option<ProjectId>, name: &str)
    -> Result<FrozenFunction, PublicError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallStatus {
    pub call: RunId,
    pub project_id: Option<ProjectId>,
    pub status: StepStatus,
    pub inputs: JsonMap,
    pub outputs: Option<JsonMap>,
    pub error: Option<PublicError>,
    pub direct: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmittedCall {
    pub call: RunId,
    pub attempt: AttemptId,
    pub project: Option<ProjectId>,
    pub function: FrozenFunction,
    pub inputs: JsonMap,
}
/// `launch` uses the common systemd guardian pipeline, including claim/start barriers.
/// An uncertain launch error must be reconciled against this identity, never relaunched.
pub trait CallGuardian: Send + Sync + 'static {
    fn launch(&self, call: AdmittedCall) -> impl Future<Output = Result<(), PublicError>> + Send;
}

#[derive(Clone)]
pub struct Calls<R, G> {
    writer: Writer,
    reads: ReadPool,
    registry: Arc<R>,
    guardian: Arc<G>,
    tasks: Arc<Mutex<TaskTracker>>,
    admission_slots: Arc<tokio::sync::Semaphore>,
}
impl<R: CallRegistry, G: CallGuardian> Calls<R, G> {
    pub fn new(writer: Writer, reads: ReadPool, registry: Arc<R>, guardian: Arc<G>) -> Self {
        Self {
            writer,
            reads,
            registry,
            guardian,
            tasks: Arc::new(Mutex::new(TaskTracker::new())),
            admission_slots: Arc::new(tokio::sync::Semaphore::new(64)),
        }
    }
    /// The coordinator retains this service and drains its handoff tasks on shutdown.
    pub async fn close(&self) {
        let tasks = {
            let tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            tasks.close();
            tasks.clone()
        };
        tasks.wait().await;
    }
    fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Result<tokio::task::JoinHandle<T>, PublicError> {
        let tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if tasks.is_closed() {
            return Err(busy("call service is closed"));
        }
        let permit = self
            .admission_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy("call handoff capacity is full"))?;
        Ok(tasks.spawn(async move {
            let _permit = permit;
            future.await
        }))
    }
    pub async fn fn_call(&self, request: FnCall) -> Result<CallStatus, PublicError> {
        if request.name == "core.external" {
            return Err(PublicError::BadRequest {
                message: "core.external never executes".into(),
            });
        }
        let project = resolve(&self.reads, request.project.clone()).await?;
        let function = self.registry.freeze(project, &request.name)?;
        check_function(&function, &request.name)?;
        validate_fields(&function.inputs, &request.inputs, "inputs")?;
        let wait = request
            .wait_seconds
            .unwrap_or(if request.direct { MAX_WAIT_SECONDS } else { 0 })
            .min(MAX_WAIT_SECONDS);
        let call = RunId::new();
        let writer = self.writer.clone();
        let guardian = Arc::clone(&self.guardian);
        let direct = request.direct;
        let (accepted, admission) = tokio::sync::oneshot::channel();
        self.spawn(async move {
            let admitted = writer.write(RetrySafety::NonIdempotent, move |tx| {
                guard_admission(tx)?;
                if let Some(project)=project {messages::resolve_project(tx.sql(),&sluice_model::ids::ProjectSelector::Id(project))?;}
                let at = timestamp(tx)?;
                tx.sql().execute("INSERT INTO calls(call_id,project_id,fn,status,inputs,direct,author,created_at) VALUES (?1,?2,?3,'pending',?4,?5,?6,?7)", (call.to_string(), project.map(|p| p.to_string()), &function.name, serde_json::to_string(&request.inputs)?, direct, &request.author, at))?;
                tx.append_record(project, Event::Call { call, name: function.name.clone(), status: StepStatus::Pending, inputs: Some(request.inputs.clone()), outputs: None, error: None, direct, author: request.author })?;
                tx.changed(project, "calls");
                if direct { admit_tx(tx, call, project, function, request.inputs).map(Some) } else { Ok(None) }
            }).await;
            match admitted {
                Err(e) => { let _ = accepted.send(Err(e)); },
                Ok(admitted) => {
                    let _ = accepted.send(Ok(()));
                    if let Some(admitted) = admitted && let Err(e) = handoff_guardian(&writer, guardian.as_ref(), admitted).await {
                        tracing::error!(%call, error = %e, "call guardian handoff requires reconciliation");
                    }
                }
            }
        })?;
        admission.await.map_err(|e| PublicError::Storage {
            message: format!("call admission task: {e}"),
        })??;
        self.wait(call, project, Duration::from_secs(wait)).await
    }
    /// Scheduler entry point. Direct calls are never part of this queue.
    pub async fn queued(&self) -> Result<Vec<(RunId, Option<ProjectId>, String)>, PublicError> {
        self.reads.snapshot(|sql| {
            let mut stmt = sql.prepare("SELECT call_id,project_id,fn FROM calls WHERE status='pending' AND direct=0 ORDER BY created_at,call_id")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,Option<String>>(1)?, r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>, _>>()?;
            rows.into_iter().map(|(c,p,n)| Ok((parse_id(c)?, p.map(parse_id).transpose()?, n))).collect()
        }).await.map_err(public)
    }
    /// Admission is one transaction; the scheduler calls this only with its active lease.
    pub async fn admit_queued(
        &self,
        call: RunId,
        project: Option<ProjectId>,
    ) -> Result<bool, PublicError> {
        let status = call_status(&self.reads, call, project).await?;
        if status.direct || status.status != StepStatus::Pending {
            return Ok(false);
        }
        let name = self
            .reads
            .snapshot(move |sql| {
                Ok(sql.query_row(
                    "SELECT fn FROM calls WHERE call_id=?1 AND project_id IS ?2",
                    (call.to_string(), project.map(|p| p.to_string())),
                    |r| r.get::<_, String>(0),
                )?)
            })
            .await
            .map_err(public)?;
        let function = self.registry.freeze(project, &name)?;
        check_function(&function, &name)?;
        validate_fields(&function.inputs, &status.inputs, "inputs")?;
        let guardian = Arc::clone(&self.guardian);
        let writer = self.writer.clone();

        self.spawn(async move {
            let admitted = writer.write(RetrySafety::NonIdempotent, move |tx| {
                guard_admission(tx)?;
                let pending: bool = tx.sql().query_row("SELECT EXISTS(SELECT 1 FROM calls WHERE call_id=?1 AND project_id IS ?2 AND status='pending' AND direct=0)", (call.to_string(), project.map(|p| p.to_string())), |r| r.get(0))?;
                if !pending { return Ok(None); }
                admit_tx(tx, call, project, function, status.inputs).map(Some)
            }).await?;
            if let Some(admitted) = admitted { handoff_guardian(&writer, guardian.as_ref(), admitted).await?; Ok(true) } else { Ok(false) }
        })?.await.map_err(|e| PublicError::Storage { message: e.to_string() })?
    }
    /// Capacity timer entry point. At most one observation per resource, with a
    /// frozen ten-second execution bound and no recursive resource dependency.
    pub async fn capacity_call(
        &self,
        project: ProjectId,
        resource: String,
    ) -> Result<Option<RunId>, PublicError> {
        let name = resource.clone();
        let declaration = self
            .reads
            .snapshot(move |sql| Ok(resources::declarations(sql, project)?.remove(&name)))
            .await
            .map_err(public)?
            .ok_or_else(|| PublicError::NotFound {
                message: "no such capacity resource".into(),
            })?;
        let resources::Capacity::Function(name) = declaration.declaration else {
            return Ok(None);
        };
        let mut function = self.registry.freeze(Some(project), &name)?;
        check_function(&function, &name)?;
        if !function.needs.is_empty()
            || function
                .inputs
                .values()
                .any(|t| !matches!(t, Type::Optional(_)))
        {
            return Err(PublicError::Invalid {
                message: "capacity fn cannot need resources or required inputs".into(),
                errors: vec![],
            });
        }
        function.timeout_seconds = Some(10);
        let call = RunId::new();
        let writer = self.writer.clone();
        let guardian = Arc::clone(&self.guardian);
        self.spawn(async move {
            let admitted = writer.write(RetrySafety::NonIdempotent, move |tx| {
                crate::drain::ensure_admission(tx, &crate::drain::Admission::Capacity { project, resource: resource.clone() })?;
                let concurrent: bool = tx.sql().query_row("SELECT EXISTS(SELECT 1 FROM attempts WHERE project_id=?1 AND phase<>'terminal' AND json_extract(request,'$.capacity.resource')=?2)", (project.to_string(), &resource), |r| r.get(0))?;
                if concurrent { return Ok(None); }
                let at = timestamp(tx)?;
                tx.sql().execute("INSERT INTO calls(call_id,project_id,fn,status,inputs,direct,author,created_at) VALUES (?1,?2,?3,'pending','{}',1,'capacity',?4)", (call.to_string(), project.to_string(), &function.name, at))?;
                tx.append_record(Some(project), Event::Call { call, name: function.name.clone(), status: StepStatus::Pending, inputs: Some(JsonMap::default()), outputs: None, error: None, direct: true, author: Some("capacity".into()) })?;
                let admitted = admit_tx(tx, call, Some(project), function, JsonMap::default())?;
                tx.sql().execute("UPDATE attempts SET request=json_set(request,'$.capacity',json(?2)) WHERE attempt_id=?1", (admitted.attempt.to_string(), serde_json::json!({"resource":resource,"revision":declaration.revision}).to_string()))?;
                Ok(Some(admitted))
            }).await?;
            if let Some(admitted) = admitted { handoff_guardian(&writer, guardian.as_ref(), admitted).await?; Ok(Some(call)) } else { Ok(None) }
        })?.await.map_err(|e| PublicError::Storage { message: e.to_string() })?
    }
    pub async fn wait(
        &self,
        call: RunId,
        project: Option<ProjectId>,
        timeout: Duration,
    ) -> Result<CallStatus, PublicError> {
        let deadline =
            tokio::time::Instant::now() + timeout.min(Duration::from_secs(MAX_WAIT_SECONDS));
        let mut subscription = self
            .reads
            .subscribe(&self.writer, vec![ChangeKey::new(project, "calls")])
            .await
            .map_err(public)?;
        loop {
            let status = call_status(&self.reads, call, project).await?;
            if matches!(status.status, StepStatus::Succeeded | StepStatus::Failed)
                || tokio::time::Instant::now() >= deadline
            {
                return Ok(status);
            }
            match tokio::time::timeout_at(deadline, subscription.wait()).await {
                Err(_) => return call_status(&self.reads, call, project).await,
                Ok(Err(e)) => return Err(public(e)),
                Ok(Ok(_)) => {}
            }
        }
    }
}
async fn handoff_guardian(
    writer: &Writer,
    guardian: &impl CallGuardian,
    admitted: AdmittedCall,
) -> Result<(), PublicError> {
    let attempt = admitted.attempt;
    let first = writer.write(RetrySafety::Idempotent, move |tx| {
        let n = tx.sql().execute("UPDATE attempts SET spawn_attempted=1 WHERE attempt_id=?1 AND phase='reserved' AND spawn_attempted=0 AND cancel_requested=0", [attempt.to_string()])?;
        if n > 0 { tx.changed(None, "calls"); }
        Ok(n > 0)
    }).await?;
    if first {
        tokio::time::timeout(Duration::from_secs(10), guardian.launch(admitted))
            .await
            .map_err(|_| PublicError::ProcessLost {
                message: "guardian handoff timed out; reconcile its existing attempt".into(),
            })??;
    }
    Ok(())
}
fn check_function(function: &FrozenFunction, requested: &str) -> Result<(), PublicError> {
    if requested == "core.external" {
        return Err(PublicError::BadRequest {
            message: "core.external never executes; set plan outputs instead".into(),
        });
    }
    if function.name != requested || function.release_id.is_empty() {
        return Err(PublicError::Invalid {
            message: "invalid frozen function identity".into(),
            errors: vec![],
        });
    }
    Ok(())
}
pub(crate) fn validate_fields(
    schema: &IndexMap<String, Type>,
    values: &JsonMap,
    path: &str,
) -> Result<(), PublicError> {
    let mut errors = vec![];
    for (name, ty) in schema {
        match values.0.get(name) {
            Some(v) => {
                if let Err(e) = check_value_at(ty, v.as_value(), &format!("{path}.{name}")) {
                    errors.extend(e.into_iter().map(|e| e.to_string()));
                }
            }
            None if matches!(ty, Type::Optional(_)) => {}
            None => errors.push(format!("{path}.{name}: required")),
        }
    }
    for name in values.0.keys() {
        if !schema.contains_key(name) {
            errors.push(format!("{path}.{name}: undeclared field"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(PublicError::Invalid {
            message: format!("invalid {path}"),
            errors,
        })
    }
}
pub(crate) fn guard_admission(tx: &WriteTransaction<'_>) -> sluice_store::Result<()> {
    let mode: String =
        tx.sql()
            .query_row("SELECT mode FROM maintenance WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
    if mode != "normal" {
        return Err(busy(format!("{mode} rejects new work")).into());
    }
    Ok(())
}
fn admit_tx(
    tx: &mut WriteTransaction<'_>,
    call: RunId,
    project: Option<ProjectId>,
    function: FrozenFunction,
    inputs: JsonMap,
) -> sluice_store::Result<AdmittedCall> {
    if let Some(project) = project {
        let fit = resources::fits(tx.sql(), project, &function.needs)?;
        if !fit.fits() {
            return Err(busy(fit.reason).into());
        }
    } else if !function.needs.is_empty() {
        return Err(PublicError::BadRequest {
            message: "resource needs require a project".into(),
        }
        .into());
    }
    let attempt = AttemptId::new();
    let at = timestamp(tx)?;
    let hash = InputsHash::of(&inputs)?;
    let frozen = serde_json::json!({"function":function,"inputs":inputs});
    tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,'reserved',?3,?4,?5)", (attempt.to_string(), project.map(|p|p.to_string()), frozen.to_string(), hash.to_string(), &at))?;
    tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,release_id,created_at) VALUES (?1,?2,?3,?4,?5)", (call.to_string(), project.map(|p|p.to_string()), attempt.to_string(), &function.release_id, &at))?;
    if let Some(job) = function
        .bundle
        .0
        .get("execution")
        .and_then(|v| v.as_value().get("job"))
        .and_then(serde_json::Value::as_str)
    {
        sluice_store::artifacts::pin_generation(tx, parse_id(job.into())?, call)?;
    }
    if let Some(project) = project {
        for (name, amount) in &function.needs {
            let amount = i64::try_from(*amount).map_err(|_| PublicError::BadRequest {
                message: "resource amount exceeds i64".into(),
            })?;
            tx.sql().execute("INSERT INTO leases(project_id,run_id,request_id,kind,group_id,scope,resource,amount,state,grant_id,created_at,granted_at) VALUES (?1,?2,?3,'needs',?2,?1,?4,?5,'held',?3,?6,?6)", (project.to_string(), call.to_string(), format!("call/{call}/{name}"), name, amount, &at))?;
        }
        tx.changed(Some(project), "resources");
    }
    tx.sql().execute(
        "UPDATE calls SET status='running',run_id=?1 WHERE call_id=?1",
        [call.to_string()],
    )?;
    let (direct, author): (bool, Option<String>) = tx.sql().query_row(
        "SELECT direct,author FROM calls WHERE call_id=?1",
        [call.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    tx.append_record(
        project,
        Event::Call {
            call,
            name: function.name.clone(),
            status: StepStatus::Running,
            inputs: None,
            outputs: None,
            error: None,
            direct,
            author,
        },
    )?;
    tx.changed(project, "calls");
    Ok(AdmittedCall {
        call,
        attempt,
        project,
        function,
        inputs,
    })
}
/// Guardian-only CAS. Identity and cleanup evidence are authenticated by the coordinator.
pub async fn claim(
    writer: &Writer,
    call: AdmittedCall,
    identity: sluice_store::attempts::GuardianIdentity,
) -> Result<bool, PublicError> {
    writer.write(RetrySafety::Idempotent, move |tx| {
        let n = tx.sql().execute("UPDATE attempts SET phase='claimed' WHERE attempt_id=?1 AND project_id IS ?2 AND phase='reserved' AND spawn_attempted=1 AND cancel_requested=0 AND EXISTS(SELECT 1 FROM runs WHERE run_id=?3 AND attempt_id=?1)", (call.attempt.to_string(), call.project.map(|p| p.to_string()), call.call.to_string()))?;
        if n == 0 { return Ok(false); }
        tx.sql().execute("UPDATE runs SET unit_name=?2,boot_id=?3,guardian_pid=?4,guardian_start=?5,cgroup=?6,socket_challenge=?7 WHERE run_id=?1", (call.call.to_string(), identity.unit_name, identity.boot_id, identity.pid, identity.start, identity.cgroup, identity.socket_challenge))?;
        tx.changed(call.project, "calls"); Ok(true)
    }).await
}
pub async fn started(
    writer: &Writer,
    call: RunId,
    attempt: AttemptId,
) -> Result<bool, PublicError> {
    writer.write(RetrySafety::Idempotent, move |tx| {
        let n = tx.sql().execute("UPDATE attempts SET phase='executing' WHERE attempt_id=?1 AND phase='claimed' AND cancel_requested=0 AND EXISTS(SELECT 1 FROM runs WHERE run_id=?2 AND attempt_id=?1)", (attempt.to_string(),call.to_string()))?;
        if n > 0 { let at=timestamp(tx)?; tx.sql().execute("UPDATE runs SET started_at=?2 WHERE run_id=?1", (call.to_string(),at))?; tx.changed(None,"calls"); }
        Ok(n>0)
    }).await
}
#[derive(Debug, Clone)]
pub struct CallCompletion {
    pub call: RunId,
    pub attempt: AttemptId,
    pub project: Option<ProjectId>,
    pub completion_id: String,
    pub outputs: JsonMap,
    pub error: Option<PublicError>,
    pub processes_gone: bool,
}
pub async fn complete(writer: &Writer, request: CallCompletion) -> Result<bool, PublicError> {
    writer.write(RetrySafety::Idempotent, move |tx| {
        if !request.processes_gone { return Err(PublicError::Invalid { message: "cleanup must be proven before call completion".into(), errors: vec![] }.into()); }
        if request.completion_id.is_empty() || request.completion_id.len()>256 { return Err(PublicError::BadRequest { message:"invalid completion identity".into() }.into()); }
        let mut stmt=tx.sql().prepare("SELECT a.request,a.phase,a.cancel_requested,r.completion_id FROM attempts a JOIN runs r USING(attempt_id) JOIN calls c ON c.run_id=r.run_id WHERE r.run_id=?1 AND r.attempt_id=?2 AND r.project_id IS ?3")?;
        let mut rows=stmt.query((request.call.to_string(),request.attempt.to_string(),request.project.map(|p|p.to_string())))?;
        let row=rows.next()?.map(|r| Ok::<_,StoreError>((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,bool>(2)?,r.get::<_,Option<String>>(3)?))).transpose()?;
        drop(rows);drop(stmt);
        let Some((frozen,phase,cancelled,completion))=row else { return Ok(false); };
        if phase=="terminal" { return Ok(completion.as_deref()==Some(&request.completion_id)); }
        let frozen:serde_json::Value=serde_json::from_str(&frozen)?;
        let function:FrozenFunction=serde_json::from_value(frozen["function"].clone())?;
        let error=if cancelled {Some(PublicError::Cancelled{message:"cancel requested".into()})}else{request.error.or_else(||validate_fields(&function.outputs,&request.outputs,"outputs").err())};
        let status=if error.is_some(){StepStatus::Failed}else{StepStatus::Succeeded};
        let result=serde_json::json!({"status":status,"outputs":request.outputs,"error":error});
        let at=timestamp(tx)?;
        tx.sql().execute("UPDATE attempts SET phase='terminal',finished_at=?2 WHERE attempt_id=?1",(request.attempt.to_string(),&at))?;
        tx.sql().execute("UPDATE runs SET result=?2,completion_id=?3,completion_ack=1,finished_at=?4 WHERE run_id=?1",(request.call.to_string(),result.to_string(),request.completion_id,&at))?;
        tx.sql().execute("UPDATE calls SET status=?2,outputs=?3,error=?4,finished_at=?5 WHERE call_id=?1",(request.call.to_string(),if error.is_some(){"failed"}else{"succeeded"},serde_json::to_string(&request.outputs)?,error.as_ref().map(serde_json::to_string).transpose()?,&at))?;
        if request.project.is_some() { resources::release_stopped_run(tx, request.call)?; }
        let (direct,author):(bool,Option<String>)=tx.sql().query_row("SELECT direct,author FROM calls WHERE call_id=?1",[request.call.to_string()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        tx.append_record(request.project,Event::Call{call:request.call,name:function.name,status,inputs:None,outputs:Some(request.outputs.clone()),error:error.clone(),direct,author})?;
        if let (Some(project), Some(capacity)) = (request.project, frozen.get("capacity")) {
            let name = capacity["resource"].as_str().ok_or_else(|| PublicError::BadRequest { message: "capacity resource missing".into() })?;
            let revision = capacity["revision"].as_i64().ok_or_else(|| PublicError::BadRequest { message: "capacity revision missing".into() })?;
            let observation = if let Some(e) = error.clone() { Err(e) } else {
                request.outputs.0.get("capacity").and_then(|v| v.as_value().as_u64()).ok_or_else(|| PublicError::FnFailure { message: "capacity must be a nonnegative integer".into() })
            };
            resources::observe_capacity(tx, project, name, revision, observation)?;
        }
        tx.changed(request.project,"calls");tx.changed(request.project,"resources"); Ok(true)
    }).await
}
pub async fn call_status(
    reads: &ReadPool,
    call: RunId,
    project: Option<ProjectId>,
) -> Result<CallStatus, PublicError> {
    reads.snapshot(move|sql|{
        let mut stmt=sql.prepare("SELECT status,inputs,outputs,error,direct FROM calls WHERE call_id=?1 AND project_id IS ?2")?;
        let mut rows=stmt.query((call.to_string(),project.map(|p|p.to_string())))?;
        let r=rows.next()?.ok_or_else(||PublicError::NotFound{message:format!("no call {call} in this scope")})?;
        Ok(CallStatus{call,project_id:project,status:serde_json::from_value(serde_json::Value::String(r.get(0)?))?,inputs:serde_json::from_str(&r.get::<_,String>(1)?)?,outputs:r.get::<_,Option<String>>(2)?.map(|s|serde_json::from_str(&s)).transpose()?,error:r.get::<_,Option<String>>(3)?.map(|s|serde_json::from_str(&s)).transpose()?,direct:r.get(4)?})
    }).await.map_err(public)
}
/// Explicit 30-day terminal-row cleanup. Feed retention cannot delete calls.
pub async fn retain_calls(writer: &Writer) -> Result<usize, PublicError> {
    writer.write(RetrySafety::Idempotent,|tx|{
        let n=tx.sql().execute("DELETE FROM calls WHERE finished_at IS NOT NULL AND julianday(finished_at)<julianday('now','-30 days') AND NOT EXISTS(SELECT 1 FROM records WHERE call_id=calls.call_id) AND (run_id IS NULL OR EXISTS(SELECT 1 FROM runs WHERE run_id=calls.run_id AND finished_at IS NOT NULL AND completion_ack=1))",[])?;
        if n>0{tx.changed(None,"calls");}Ok(n)
    }).await
}
pub(crate) async fn resolve(
    reads: &ReadPool,
    selector: Option<sluice_model::ids::ProjectSelector>,
) -> Result<Option<ProjectId>, PublicError> {
    reads
        .snapshot(move |sql| {
            selector
                .map(|s| messages::resolve_project(sql, &s))
                .transpose()
        })
        .await
        .map_err(public)
}
pub(crate) fn public(e: StoreError) -> PublicError {
    e.into_public(true)
}
pub(crate) fn parse_id<T: std::str::FromStr>(s: String) -> sluice_store::Result<T>
where
    T::Err: std::fmt::Display,
{
    s.parse()
        .map_err(|e: T::Err| StoreError::InvalidDatabase(e.to_string()))
}
pub(crate) fn timestamp(tx: &WriteTransaction<'_>) -> sluice_store::Result<String> {
    Ok(tx
        .sql()
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
            r.get(0)
        })?)
}
pub(crate) fn busy(message: impl Into<String>) -> PublicError {
    PublicError::Busy {
        message: message.into(),
        retryable: false,
    }
}

/// Coordinator dispatch entry point. `None` leaves commands to the other owners.
/// The coordinator must run drain::check_command before its whole dispatch table,
/// and ensure_admission inside every plan reservation transaction.
pub async fn dispatch_p3_04<
    R: CallRegistry,
    G: CallGuardian,
    V: crate::verify::VerificationRegistry,
>(
    calls: &Calls<R, G>,
    writer: &Writer,
    reads: &ReadPool,
    registry: Arc<V>,
    request: sluice_model::commands::CommandRequest,
) -> Result<Option<sluice_model::commands::CommandReply>, PublicError> {
    use sluice_model::{
        commands::{CommandReply, CommandRequest},
        rpc::JsonValue,
    };
    let data = |value: serde_json::Value| JsonValue::try_from(value).map(CommandReply::Data);
    let reply = match request {
        CommandRequest::FnCall(request) => data(
            serde_json::to_value(calls.fn_call(request).await?).map_err(|e| {
                PublicError::Storage {
                    message: e.to_string(),
                }
            })?,
        )?,
        CommandRequest::CallStatus { call, project } => data(
            serde_json::to_value(call_status(reads, call, resolve(reads, project).await?).await?)
                .map_err(|e| PublicError::Storage {
                message: e.to_string(),
            })?,
        )?,
        CommandRequest::Drain { projects, author } => {
            let paused =
                crate::drain::drain(writer, projects, author.unwrap_or_else(|| "cli".into()))
                    .await?;
            data(serde_json::json!({"paused":paused,"status":crate::drain::status(reads).await?}))?
        }
        CommandRequest::Release { author } => data(
            serde_json::json!({"released":crate::drain::release(writer,author.unwrap_or_else(||"cli".into())).await?}),
        )?,
        CommandRequest::Verify { project } => data(
            serde_json::to_value(crate::verify::verify(reads, registry, project).await?).map_err(
                |e| PublicError::Storage {
                    message: e.to_string(),
                },
            )?,
        )?,
        CommandRequest::Next(request) => {
            CommandReply::Next(crate::watch::next_command(writer, reads, request).await?)
        }
        _ => return Ok(None),
    };
    Ok(Some(reply))
}
