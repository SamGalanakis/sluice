//! One home writer, command service and guardian broker.
use crate::{
    calls::{self, Calls},
    dispatch::{Catalog, Hooks, InputSetter, ResourceSettings},
    execution::{CallLauncher, ExecutionHost},
    scheduler,
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sluice_model::{
    RuntimeApi,
    commands::*,
    edit::{self, EditSnapshot, PlanEdit},
    error::PublicError,
    events::{ChangeBatch, ChangeCursor},
    gates::CachedResources,
    ids::*,
    plan::{Plan, Snapshot},
    rpc::{self, JsonMap, RpcReply, RpcRequest, RpcResult, RunCapability},
};
use sluice_process::{
    guardian::{AdoptionAttempt, adopt_attempt},
    identity::ProcessIdentity,
    journal::{AttemptKey, CompletionJournal, PayloadResult},
    socket::{
        self, CoordinatorCommand, CoordinatorLink, CoordinatorReply, DurableAck, GuardianIdentity,
        Reply, Request, SubmissionSnapshot,
    },
};
use sluice_store::{
    ReadPool, RetrySafety, StoreError, Writer, artifacts, attempts,
    plans::{self, PlanContext},
    projects, records, resources,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

pub fn executor() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}
fn storage(e: impl std::fmt::Display) -> PublicError {
    PublicError::Storage {
        message: e.to_string(),
    }
}
fn conflict(message: impl Into<String>) -> PublicError {
    PublicError::Conflict {
        message: message.into(),
        current_rev: None,
    }
}
fn data(value: impl Serialize) -> Result<CommandReply, PublicError> {
    Ok(CommandReply::Data(
        serde_json::to_value(value).map_err(storage)?.try_into()?,
    ))
}

struct Inner<H: ExecutionHost> {
    writer: Writer,
    reads: ReadPool,
    home: PathBuf,
    home_id: HomeId,
    catalog: Arc<Catalog>,
    host: Arc<H>,
    calls: Calls<Catalog, CallLauncher<H>>,
}
type StoredAttempt = (Option<String>, Option<String>, i64, i64, String);
pub struct Coordinator<H: ExecutionHost> {
    inner: Arc<Inner<H>>,
}
impl<H: ExecutionHost> Clone for Coordinator<H> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl<H: ExecutionHost> Coordinator<H> {
    /// Writer::open owns the home flock until the actor connection is closed.
    pub async fn open(home: PathBuf, catalog: Catalog, host: H) -> Result<Self, PublicError> {
        sluice_process::host::guard_scratch_home(&home)?;
        let path = home.clone();
        let writer = tokio::task::spawn_blocking(move || Writer::open(path))
            .await
            .map_err(storage)?
            .map_err(|e| e.into_public(false))?;
        let path = home.clone();
        let reads = tokio::task::spawn_blocking(move || ReadPool::open(path, 4))
            .await
            .map_err(storage)?
            .map_err(|e| e.into_public(true))?;
        let home_id = reads
            .snapshot(|sql| {
                let id: String =
                    sql.query_row("SELECT home_id FROM home_meta WHERE singleton=1", [], |r| {
                        r.get(0)
                    })?;
                calls::parse_id(id)
            })
            .await
            .map_err(|e| e.into_public(true))?;
        // The flock proves every earlier coordinator connection and lease is gone.
        writer.write(RetrySafety::Idempotent,|tx|{if tx.sql().execute("UPDATE maintenance SET scheduler_owner=NULL,scheduler_lease_until=NULL WHERE scheduler_owner IS NOT NULL",[])?>0{tx.changed(None,"scheduler");}Ok(())}).await?;
        let catalog = Arc::new(catalog);
        let host = Arc::new(host);
        let calls = Calls::new(
            writer.clone(),
            reads.clone(),
            catalog.clone(),
            Arc::new(CallLauncher {
                host: host.clone(),
                home: home_id,
                writer: writer.clone(),
            }),
        );
        let broker = Self {
            inner: Arc::new(Inner {
                writer,
                reads,
                home,
                home_id,
                catalog,
                host,
                calls,
            }),
        };
        artifacts::recover(broker.writer(), broker.home())
            .await
            .map_err(|e| e.into_public(false))?;
        Ok(broker)
    }
    pub fn writer(&self) -> &Writer {
        &self.inner.writer
    }
    pub fn reads(&self) -> &ReadPool {
        &self.inner.reads
    }
    pub fn home(&self) -> &Path {
        &self.inner.home
    }
    pub fn home_id(&self) -> HomeId {
        self.inner.home_id
    }
    pub fn host(&self) -> &H {
        &self.inner.host
    }
    pub fn catalog(&self) -> &Catalog {
        &self.inner.catalog
    }
    pub fn calls(&self) -> &Calls<Catalog, CallLauncher<H>> {
        &self.inner.calls
    }
    pub async fn context(&self, project: ProjectId) -> Result<PlanContext, PublicError> {
        let catalog = self.inner.catalog.clone();
        self.reads()
            .snapshot(move |sql| context(sql, project, &catalog))
            .await
            .map_err(|e| e.into_public(true))
    }
    pub async fn projects(&self) -> Result<Vec<ProjectId>, PublicError> {
        self.reads().snapshot(|sql|{let mut stmt=sql.prepare("SELECT project_id FROM projects WHERE deleted_at IS NULL ORDER BY created_at,project_id")?;let rows=stmt.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;rows.into_iter().map(calls::parse_id).collect()}).await.map_err(|e|e.into_public(true))
    }
    pub async fn project_versions(&self) -> Result<BTreeMap<ProjectId, i64>, PublicError> {
        self.reads().snapshot(|sql|{let mut stmt=sql.prepare("SELECT p.project_id,coalesce(sum(v.version),0) FROM projects p LEFT JOIN change_versions v ON v.project_id=p.project_id WHERE p.deleted_at IS NULL GROUP BY p.project_id")?;let rows=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?.collect::<Result<Vec<_>,_>>()?;rows.into_iter().map(|(id,n)|Ok((calls::parse_id(id)?,n))).collect()}).await.map_err(|e|e.into_public(true))
    }
    pub async fn capacity_resources(&self, project: ProjectId) -> Result<Vec<String>, PublicError> {
        self.reads()
            .snapshot(move |sql| {
                Ok(resources::capacity_observations(sql, project)?
                    .into_iter()
                    .map(|r| r.name)
                    .collect())
            })
            .await
            .map_err(|e| e.into_public(true))
    }
    pub async fn scheduler_owner(&self) -> Result<Option<String>, PublicError> {
        self.reads()
            .snapshot(|sql| {
                Ok(sql.query_row(
                    "SELECT scheduler_owner FROM maintenance WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .map_err(|e| e.into_public(true))
    }
    pub async fn acquire_scheduler(&self, owner: String) -> Result<(), PublicError> {
        if owner.is_empty() || owner.len() > 256 {
            return Err(conflict("invalid scheduler owner"));
        }
        self.writer().write(RetrySafety::NonIdempotent,move|tx|{let n=tx.sql().execute("UPDATE maintenance SET scheduler_owner=?1 WHERE singleton=1 AND scheduler_owner IS NULL",[owner])?;if n==0{return Err(conflict("scheduler lease already held").into());}tx.changed(None,"scheduler");Ok(())}).await
    }
    pub async fn release_scheduler(&self, owner: String) -> Result<(), PublicError> {
        self.writer().write(RetrySafety::Idempotent,move|tx|{if tx.sql().execute("UPDATE maintenance SET scheduler_owner=NULL,scheduler_lease_until=NULL WHERE singleton=1 AND scheduler_owner=?1",[owner])?>0{tx.changed(None,"scheduler");}Ok(())}).await
    }
    pub async fn command(&self, request: CommandRequest) -> Result<CommandReply, PublicError> {
        crate::drain::check_command(self.reads(), &request).await?;
        if let Some(reply) = calls::dispatch_p3_04(
            self.calls(),
            self.writer(),
            self.reads(),
            self.inner.catalog.clone(),
            request.clone(),
        )
        .await?
        {
            return Ok(reply);
        }
        let catalog = self.inner.catalog.clone();
        match request {
            CommandRequest::ProjectsList=>{let values=self.reads().snapshot(|sql|{let mut q=sql.prepare("SELECT project_id,name FROM projects WHERE deleted_at IS NULL ORDER BY name")?;let rows=q.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;rows.into_iter().map(|(id,name)|Ok(ProjectIdentity{project_id:calls::parse_id(id)?,name:calls::parse_id(name)?})).collect::<sluice_store::Result<Vec<_>>>()}).await.map_err(|e|e.into_public(true))?;Ok(CommandReply::Projects(values))},
            CommandRequest::ProjectCreate{name,description,icon,resources,author}=>{
                let project=self.writer().write(RetrySafety::NonIdempotent,move|tx|projects::project_create(tx,projects::CreateProject{name,description,icon:icon.map(|s|projects::Icon::text(&s)).transpose()?,resources:Some(serde_json::to_value(resources)?),author:author.unwrap_or_else(||"cli".into())},&projects::EmptyPlanInitializer,&ResourceSettings((*catalog).clone()))).await?;
                artifacts::recover(self.writer(),self.home()).await.map_err(|e|e.into_public(false))?; Ok(CommandReply::Project(ProjectIdentity{project_id:project.project_id,name:project.name}))
            },
            CommandRequest::ProjectUpdate(update)=>{
                let project=self.writer().write(RetrySafety::NonIdempotent,move|tx|projects::project_update(tx,&update.project,projects::UpdateProject{new_name:update.new_name,description:update.description,icon:update.icon.map(projects::Icon::try_from).transpose()?,resources:update.resources.map(serde_json::to_value).transpose()?,paused:update.paused,archived:update.archived,expected_settings_rev:update.expected_settings_rev,reason:update.reason,author:update.author.unwrap_or_else(||"cli".into())},&ResourceSettings((*catalog).clone()))).await?;artifacts::recover(self.writer(),self.home()).await.map_err(|e|e.into_public(false))?; Ok(CommandReply::Project(ProjectIdentity{project_id:project.project_id,name:project.name}))
            },
            CommandRequest::PlanGet{project}=>self.reads().snapshot(move|sql|{let id=messages_project(sql,&project)?;let ctx=context(sql,id,&catalog)?;Ok(json!({"project":projects_identity(sql,id)?,"rev":ctx.revision,"plan":ctx.plan.document()}))}).await.map_err(|e|e.into_public(true)).and_then(data),
            CommandRequest::Status{project,selection:_}=>self.reads().snapshot(move|sql|{
                let id=messages_project(sql,&project)?;let ctx=context(sql,id,&catalog)?;let state=plans::read_state(sql,id)?;
                let mut q=sql.prepare("SELECT step_id,json_object('status',status,'outputs',json(outputs),'error',json(error),'run_ids',json(run_ids),'done',done,'total',total,'instances',json(instances),'manual',manual) FROM steps WHERE project_id=?1 ORDER BY position")?;
                let rows=q.query_map([id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
                let mut steps=serde_json::Map::new();for (id,row) in rows{steps.insert(id,serde_json::from_str(&row)?);}
                for candidate in resources::admit_order(sql,id,&ctx.plan)? {
                    let fit=resources::fits(sql,id,&candidate.needs)?;
                    if !fit.fits() && let Some(Value::Object(step))=steps.get_mut(candidate.step.as_str()) {
                            step.insert("queued".into(),json!(fit.blocked));
                            step.insert("waiting".into(),json!([format!("queued: {}",fit.reason)]));
                    }
                }
                let outputs=JsonMap(ctx.plan.outputs().iter().filter_map(|(n,r)|match sluice_model::gates::resolve_reference(&ctx.plan,&state,r){sluice_model::types::BoundValue::Ready(v)=>Some((n.clone(),v)),_=>None}).collect::<indexmap::IndexMap<_,_>>());
                Ok(json!({"project":projects_identity(sql,id)?,"rev":ctx.revision,"inputs":state.inputs,"steps":steps,"outputs":outputs,"resources":resources::status(sql,id,&ctx.plan)?.into_iter().map(|(n,r)|(n,json!({"capacity":r.resource.capacity,"held":r.held,"queued":r.queued,"error":r.resource.error}))).collect::<BTreeMap<_,_>>()}))
            }).await.map_err(|e|e.into_public(true)).and_then(data),
            CommandRequest::StepRetry(request)=>self.writer().write(RetrySafety::NonIdempotent,move|tx|{crate::drain::ensure_admission(tx,&crate::drain::Admission::Plan)?;let id=messages_project(tx.sql(),&request.project)?;let ctx=context(tx.sql(),id,&catalog)?;Ok(CommandReply::Retry(plans::step_retry(tx,&ctx,request,&mut Hooks)?))}).await,
            CommandRequest::StepCancel(request)=>self.writer().write(RetrySafety::NonIdempotent,move|tx|{let id=messages_project(tx.sql(),&request.project)?;let ctx=context(tx.sql(),id,&catalog)?;plans::step_cancel(tx,&ctx,request)?;Ok(CommandReply::Ack)}).await,
            CommandRequest::StepSubmit(request)=>self.writer().write(RetrySafety::NonIdempotent,move|tx|{let version=attempts::step_submit(tx,request)?;if version.is_none(){return Err(conflict("stale submission").into());}Ok(CommandReply::Ack)}).await,
            CommandRequest::Submission{run}=>data(self.submissions(run).await?),
            CommandRequest::StepSetOutput(request)=>self.writer().write(RetrySafety::NonIdempotent,move|tx|{let id=messages_project(tx.sql(),&request.project)?;let ctx=context(tx.sql(),id,&catalog)?;plans::step_set_output(tx,&ctx,request)?;Ok(CommandReply::Ack)}).await,
            CommandRequest::PlanSetInput(request) => self.writer().write(RetrySafety::NonIdempotent, move |tx| {
                crate::drain::ensure_admission(tx, &crate::drain::Admission::Plan)?;
                let id = messages_project(tx.sql(), &request.project)?;
                let ctx = context(tx.sql(), id, &catalog)?;
                if request.edit.dry_run {
                    return Err(PublicError::not_implemented("input dry run").into());
                }
                if request.edit.expected.is_some_and(|r| r != ctx.revision) {
                    return Err(conflict("plan revision changed").into());
                }
                plans::set_input(tx, &ctx, &request.name, request.value, request.edit.author.unwrap_or_default(), request.edit.reason)?;
                Ok(CommandReply::Ack)
            }).await,
            CommandRequest::MessagePost(request)=>self.writer().write(RetrySafety::NonIdempotent,move|tx|{let id=messages_project(tx.sql(),&request.project)?;let ctx=context(tx.sql(),id,&catalog)?;let m=sluice_store::messages::message_post(tx,request,&InputSetter(ctx))?;Ok(CommandReply::Posted{id:m.id})}).await,
            CommandRequest::Messages(request)=>self.reads().snapshot(move|sql|{let id=messages_project(sql,&request.project)?;let messages=sluice_store::messages::messages(sql,id,request.view,request.thread.as_deref(),request.since,"cli")?;Ok(CommandReply::Messages(MessagePage{project:projects_identity(sql,id)?,last_id:messages.last().map(|m|m.id),messages}))}).await.map_err(|e|e.into_public(true)),
            CommandRequest::AcquireLease(request)=>self.writer().write(RetrySafety::Idempotent,move|tx|{let lease=resources::request_lease_keyed(tx,request.run,&request.resource,request.amount,&format!("callback/{}/{}",request.run,request.request_id))?;let state=resources::leases(tx.sql(),run_project(tx.sql(),request.run)?)?.into_iter().find(|l|l.id==lease).ok_or_else(||conflict("lease missing"))?.state;Ok(CommandReply::Lease{lease,state})}).await,
            CommandRequest::ReleaseLease(request)=>self.writer().write(RetrySafety::Idempotent,move|tx|{resources::release_lease(tx,request.lease,request.run)?;Ok(CommandReply::Ack)}).await,
            CommandRequest::RegisterCompletionAction(request)=>self.writer().write(RetrySafety::Idempotent,move|tx|{if !attempts::register_completion_action(tx,request,&Hooks)?{return Err(conflict("stale action registration").into());}Ok(CommandReply::Ack)}).await,
            CommandRequest::LogRead(request)=>self.reads().snapshot(move|sql|{let project=request.project.as_ref().map(|p|messages_project(sql,p)).transpose()?;Ok(CommandReply::Records(records::read_records(sql,project,&records::RecordFilter{since:request.since_seq,kinds:request.kinds.unwrap_or_default(),threads:request.threads.unwrap_or_default(),limit:request.limit})?.into_page()?))}).await.map_err(|e|e.into_public(true)),
            other=>{
                let project=edit_project(&other).ok_or_else(||PublicError::not_implemented("command dispatch extension"))?;
                let edit=PlanEdit::try_from(other)?;
                let (id,prepared)=self.reads().snapshot(move|sql|{
                    let id=messages_project(sql,&project)?;let ctx=context(sql,id,&catalog)?;let state=plans::read_state(sql,id)?;let snapshot=Snapshot{revision:ctx.revision,document:ctx.plan.document().clone()};
                    let prepared=edit::prepare_edit(&EditSnapshot{snapshot:&snapshot,state:&state,signatures:catalog.as_ref(),recipes:&Default::default(),resources:&CachedResources::default(),limits:&resource_limits(sql,id)?,prune_eligible:None},edit)?;Ok((id,prepared))
                }).await.map_err(|e|e.into_public(true))?;
                self.writer().write(RetrySafety::NonIdempotent,move|tx|{crate::drain::ensure_admission(tx,&crate::drain::Admission::Plan)?;if prepared.dry_run{return Ok(CommandReply::Preview(prepared.preview));}Ok(CommandReply::Edit(plans::apply_edit(tx,id,prepared)?))}).await
            }
        }
    }
    pub async fn submissions(&self, run: RunId) -> Result<SubmissionSnapshot, PublicError> {
        self.reads()
            .snapshot(move |sql| {
                let row: Option<(i64, String)> = sql
                    .query_row(
                        "SELECT version,outputs FROM submissions WHERE run_id=?1",
                        [run.to_string()],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                Ok(match row {
                    Some((version, fields)) => SubmissionSnapshot {
                        version: Some(version as u64),
                        fields: serde_json::from_str(&fields)?,
                    },
                    None => SubmissionSnapshot {
                        version: None,
                        fields: JsonMap::default(),
                    },
                })
            })
            .await
            .map_err(|e| e.into_public(true))
    }
    async fn authenticate(
        &self,
        id: &AttemptKey,
        capability: Option<&RunCapability>,
    ) -> Result<(), PublicError> {
        if id.home != self.home_id() {
            return Err(conflict("wrong home identity"));
        }
        let id = id.clone();
        let capability = capability.cloned();
        self.reads().snapshot(move|sql|{
            let row:Option<StoredAttempt>=sql.query_row("SELECT r.project_id,r.step_id,r.generation,r.work_generation,a.request FROM runs r JOIN attempts a USING(attempt_id) WHERE r.run_id=?1 AND r.attempt_id=?2",(id.run.to_string(),id.attempt.to_string()),|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let Some((project,step,generation,work,request))=row else{return Err(conflict("unknown attempt").into());};
            let request:Value=serde_json::from_str(&request)?;let saved=if step.is_some(){&request["provenance"]["runtime"]["capability"]}else{&request["function"]["bundle"]["capability"]};
            let expected:RunCapability=serde_json::from_value(saved.clone())?;
            if project!=id.project.map(|p|p.to_string()) || step!=id.step.as_ref().map(ToString::to_string) || generation as u64!=id.generation.0 || work as u64!=id.work.0 || capability.as_ref()!=Some(&expected){return Err(conflict("attempt identity or capability mismatch").into());}Ok(())
        }).await.map_err(|e|e.into_public(false))
    }
    pub async fn guardian(
        &self,
        command: CoordinatorCommand,
        capability: Option<&RunCapability>,
    ) -> Result<CoordinatorReply, PublicError> {
        let id = guardian_key(&command).clone();
        self.authenticate(&id, capability).await?;
        match command {
            CoordinatorCommand::Claim(g) => {
                if g.unit != sluice_process::systemd::TransientService::for_test(id.run).name()
                    || g.process.pid == 0
                    || g.socket_challenge.is_empty()
                {
                    return Err(conflict("invalid guardian identity"));
                }
                let home = self.home_id();
                let accepted=self.writer().write(RetrySafety::Idempotent,move|tx|{
                    let (phase,cancel):(String,bool)=tx.sql().query_row("SELECT phase,cancel_requested FROM attempts WHERE attempt_id=?1",[id.attempt.to_string()],|r|Ok((r.get(0)?,r.get(1)?)))?;
                    if phase=="terminal" || cancel {return Ok(false);}
                    let stored=stored_guardian(tx.sql(),&id,home)?;
                    if let Some(old)=stored{return Ok(old==g);}
                    let identity=store_guardian(&g);
                    if id.step.is_some(){attempts::claim(tx,&step_identity(&id)?,identity)}else{
                        let n=tx.sql().execute("UPDATE attempts SET phase='claimed' WHERE attempt_id=?1 AND phase='reserved' AND spawn_attempted=1 AND cancel_requested=0",[id.attempt.to_string()])?;
                        if n>0{persist_guardian(tx,&id,identity)?;tx.changed(id.project,"calls");}Ok(n>0)
                    }
                }).await?;
                Ok(CoordinatorReply::Claimed(accepted))
            }
            CoordinatorCommand::Started {
                invocation,
                executor,
                ..
            } => {
                self.started(id, invocation, executor).await?;
                Ok(CoordinatorReply::Started)
            }
            CoordinatorCommand::CancelIntent(_) => {
                let cancelled = self
                    .reads()
                    .snapshot(move |sql| {
                        Ok(sql.query_row(
                            "SELECT cancel_requested FROM attempts WHERE attempt_id=?1",
                            [id.attempt.to_string()],
                            |r| r.get(0),
                        )?)
                    })
                    .await
                    .map_err(|e| e.into_public(true))?;
                Ok(CoordinatorReply::CancelIntent(cancelled))
            }
            CoordinatorCommand::Messages {
                after,
                through,
                limit,
                ..
            } => {
                if limit == 0 || limit > 128 || after.0 < 0 {
                    return Err(conflict("invalid delivery window"));
                }
                let messages=self.writer().write(RetrySafety::Idempotent,move|tx|{
                    let Some(project)=id.project else{return Ok(vec![]);};let Some(step)=id.step else{return Ok(vec![]);};
                    let mut q=tx.sql().prepare("SELECT id FROM messages WHERE project_id=?1 AND \"to\"=?2 AND id>?3 AND (?4 IS NULL OR id<=?4) ORDER BY id LIMIT ?5")?;
                    let ids=q.query_map((project.to_string(),step.as_str(),after.0,through.map(|m|m.0),limit),|r|r.get::<_,i64>(0))?.collect::<Result<Vec<_>,_>>()?;drop(q);
                    let mut out=vec![];for message in ids{
                        tx.sql().execute("INSERT INTO message_deliveries(project_id,run_id,message_id,assigned_at) VALUES (?1,?2,?3,strftime('%Y-%m-%dT%H:%M:%fZ','now')) ON CONFLICT DO NOTHING",(project.to_string(),id.run.to_string(),message))?;
                        out.push(socket::DeliveryMessage{id:MessageId(message),body:serde_json::to_value(sluice_store::messages::message(tx.sql(),project,MessageId(message))?)?.try_into()?});
                    }
                    if !out.is_empty(){tx.changed(Some(project),"messages");}Ok(out)
                }).await?;
                Ok(CoordinatorReply::Messages(messages))
            }
            CoordinatorCommand::DeliverAck { ack, .. } => {
                self.writer()
                    .write(RetrySafety::Idempotent, move |tx| {
                        require_invocation(tx.sql(), &id, ack.invocation)?;
                        if let Some(project) = id.project {
                            sluice_store::messages::acknowledge_delivery(
                                tx,
                                project,
                                id.run,
                                ack.message,
                            )?;
                        }
                        Ok(())
                    })
                    .await?;
                Ok(CoordinatorReply::Ack)
            }
            CoordinatorCommand::Submissions(_) => Ok(CoordinatorReply::Submissions(
                self.submissions(id.run).await?,
            )),
            CoordinatorCommand::Complete(journal) => {
                Ok(CoordinatorReply::Completed(self.complete(*journal).await?))
            }
            CoordinatorCommand::Callback { request, .. } => {
                if request.protocol != rpc::PROTOCOL_VERSION
                    || request.run_capability.as_ref() != capability
                {
                    return Err(conflict("callback capability mismatch"));
                }
                self.callback(id, *request)
                    .await
                    .map(|r| CoordinatorReply::Callback(Box::new(r)))
            }
        }
    }
    async fn started(
        &self,
        id: AttemptKey,
        invocation: InvocationId,
        executor: ProcessIdentity,
    ) -> Result<(), PublicError> {
        self.writer().write(RetrySafety::Idempotent,move|tx|{
            let (phase,request):(String,String)=tx.sql().query_row("SELECT phase,request FROM attempts WHERE attempt_id=?1",[id.attempt.to_string()],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let mut request:Value=serde_json::from_str(&request)?;let evidence=json!({"invocation":invocation,"executor":executor});
            let starts=request.as_object_mut().ok_or_else(||conflict("frozen request shape"))?.entry("runtime_starts").or_insert(json!([])).as_array_mut().ok_or_else(||conflict("start evidence shape"))?;
            if let Some(old)=starts.iter().find(|v|v["invocation"]==json!(invocation)){if *old!=evidence{return Err(conflict("start evidence changed").into());}return Ok(());}
            if starts.len()>=1024{return Err(conflict("too many invocation starts").into());}starts.push(evidence);
            if phase=="claimed" {
                if id.step.is_some(){if !attempts::started(tx,&step_identity(&id)?,&mut Hooks)?{return Err(conflict("start refused").into());}}
                else{let n=tx.sql().execute("UPDATE attempts SET phase='executing' WHERE attempt_id=?1 AND cancel_requested=0",[id.attempt.to_string()])?;if n==0{return Err(conflict("start cancelled").into());}tx.sql().execute("UPDATE runs SET started_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE run_id=?1",[id.run.to_string()])?;}
            }else if phase!="executing"{return Err(conflict("attempt cannot acknowledge start").into());}
            tx.sql().execute("UPDATE attempts SET request=?2 WHERE attempt_id=?1",(id.attempt.to_string(),request.to_string()))?;tx.changed(id.project,"status");Ok(())
        }).await
    }
    async fn callback(
        &self,
        id: AttemptKey,
        request: RpcRequest,
    ) -> Result<CommandReply, PublicError> {
        // The bearer grants callbacks for this admitted identity only. Never expose
        // arbitrary project editing, calls or privileged capacity admission to it.
        match &request.command {
            CommandRequest::StepSubmit(s)
                if Some(s.project) == id.project
                    && Some(&s.step) == id.step.as_ref()
                    && s.run == id.run => {}
            CommandRequest::Submission { run } if *run == id.run => {}
            CommandRequest::MessagePost(m)
                if m.run == Some(id.run)
                    && m.project
                        == ProjectSelector::Id(
                            id.project
                                .ok_or_else(|| conflict("callback needs a project"))?,
                        )
                    && m.from.as_deref() == id.step.as_ref().map(StepId::as_str) => {}
            CommandRequest::AcquireLease(l) if l.run == id.run && id.project.is_some() => {}
            CommandRequest::ReleaseLease(l) if l.run == id.run => {}
            CommandRequest::RegisterCompletionAction(a)
                if a.run == id.run && Some(a.project) == id.project => {}
            _ => return Err(conflict("callback command is outside the run authority")),
        }
        let key = request.request_id.0;
        let command = request.command;
        // Callback mutations and their reply cache share one transaction. Dispatch
        // through the same adapters while keeping the immutable request for replay.
        let catalog = self.inner.catalog.clone();
        self.writer()
            .write(RetrySafety::Idempotent, move |tx| {
                let raw: String = tx.sql().query_row(
                    "SELECT request FROM attempts WHERE attempt_id=?1",
                    [id.attempt.to_string()],
                    |r| r.get(0),
                )?;
                let mut frozen: Value = serde_json::from_str(&raw)?;
                let cache = frozen
                    .as_object_mut()
                    .ok_or_else(|| conflict("attempt request shape"))?
                    .entry("runtime_callbacks")
                    .or_insert(json!({}))
                    .as_object_mut()
                    .ok_or_else(|| conflict("callback cache shape"))?;
                if let Some(saved) = cache.get(&key) {
                    if saved["command"] != serde_json::to_value(&command)? {
                        return Err(conflict("callback request ID reused").into());
                    }
                    return Ok(serde_json::from_value(saved["reply"].clone())?);
                }
                if cache.len() >= 16384 {
                    return Err(conflict("callback cache limit").into());
                }
                let reply = match command.clone() {
                    CommandRequest::StepSubmit(s) => {
                        if attempts::step_submit(tx, s)?.is_none() {
                            return Err(conflict("stale submission").into());
                        }
                        CommandReply::Ack
                    }
                    CommandRequest::Submission { run } => {
                        let row: Option<String> = tx
                            .sql()
                            .query_row(
                                "SELECT outputs FROM submissions WHERE run_id=?1",
                                [run.to_string()],
                                |r| r.get(0),
                            )
                            .optional()?;
                        CommandReply::Data(serde_json::from_str(row.as_deref().unwrap_or("{}"))?)
                    }
                    CommandRequest::MessagePost(m) => {
                        let ctx =
                            context(tx.sql(), id.project.expect("validated project"), &catalog)?;
                        CommandReply::Posted {
                            id: sluice_store::messages::message_post(tx, m, &InputSetter(ctx))?.id,
                        }
                    }
                    CommandRequest::AcquireLease(l) => {
                        let lease = resources::request_lease_keyed(
                            tx,
                            id.run,
                            &l.resource,
                            l.amount,
                            &format!("callback/{}/{key}", id.run),
                        )?;
                        let state = resources::leases(tx.sql(), run_project(tx.sql(), l.run)?)?
                            .into_iter()
                            .find(|l| l.id == lease)
                            .ok_or_else(|| conflict("lease missing"))?
                            .state;
                        CommandReply::Lease { lease, state }
                    }
                    CommandRequest::ReleaseLease(l) => {
                        resources::release_lease(tx, l.lease, id.run)?;
                        CommandReply::Ack
                    }
                    CommandRequest::RegisterCompletionAction(a) => {
                        if !attempts::register_completion_action(tx, a, &Hooks)? {
                            return Err(conflict("stale completion action").into());
                        }
                        CommandReply::Ack
                    }
                    _ => return Err(conflict("unsupported callback").into()),
                };
                cache.insert(key, json!({"command":command,"reply":reply}));
                tx.sql().execute(
                    "UPDATE attempts SET request=?2 WHERE attempt_id=?1",
                    (id.attempt.to_string(), frozen.to_string()),
                )?;
                tx.changed(id.project, "status");
                Ok(reply)
            })
            .await
    }
    pub async fn complete(&self, journal: CompletionJournal) -> Result<DurableAck, PublicError> {
        journal.validate(&journal.identity).map_err(storage)?;
        if journal.identity.home != self.home_id() {
            return Err(conflict("completion home mismatch"));
        }
        let id = &journal.identity;
        // Real hosts independently recheck containment. Fake hosts own their proof.
        if !self.host().cleanup_valid(&journal).await? {
            return Err(conflict("payload cleanup is not proven"));
        }
        for start in &journal.starts {
            self.started(id.clone(), start.invocation, start.executor.clone())
                .await?;
        }
        let snapshot = self.submissions(id.run).await?;
        if snapshot.version != journal.submission_version || snapshot.fields != journal.submissions
        {
            return Err(conflict("completion submissions changed"));
        }
        let completion_id = journal.completion_id.clone();
        let run = id.run;
        if id.step.is_some() {
            let catalog = self.inner.catalog.clone();
            self.writer()
                .write(RetrySafety::Idempotent, move |tx| {
                    let id = step_identity(&journal.identity)?;
                    let ctx = context(tx.sql(), id.project, &catalog)?;
                    for ack in &journal.delivery_acks {
                        require_invocation(tx.sql(), &journal.identity, ack.invocation)?;
                        sluice_store::messages::acknowledge_delivery(
                            tx,
                            id.project,
                            id.run,
                            ack.message,
                        )?;
                    }
                    let (kind, outputs) = completion_kind(journal.result);
                    if attempts::complete(
                        tx,
                        &ctx,
                        attempts::Complete {
                            identity: id,
                            completion_id: journal.completion_id,
                            kind,
                            outputs,
                            processes_gone: true,
                            submission_version: journal.submission_version,
                        },
                        &mut Hooks,
                    )?
                    .is_none()
                    {
                        return Err(conflict("stale completion").into());
                    }
                    Ok(())
                })
                .await?;
        } else {
            let (_, outputs) = completion_kind(journal.result.clone());
            let error = match journal.result {
                PayloadResult::Succeeded(_) => None,
                PayloadResult::Failed(e) => Some(e),
                PayloadResult::Cancelled(message) => Some(PublicError::Cancelled { message }),
                PayloadResult::Rejected(message) => Some(PublicError::FnFailure { message }),
                PayloadResult::Lost(message) | PayloadResult::Unknown(message) => {
                    Some(PublicError::ProcessLost { message })
                }
            };
            if !calls::complete(
                self.writer(),
                calls::CallCompletion {
                    call: run,
                    attempt: id.attempt,
                    project: id.project,
                    completion_id: completion_id.clone(),
                    outputs,
                    error,
                    processes_gone: true,
                },
            )
            .await?
            {
                return Err(conflict("stale call completion"));
            }
        }
        Ok(DurableAck { run, completion_id })
    }
    pub async fn adopt(&self) -> Result<(), PublicError> {
        let home = self.home().to_path_buf();
        let home_id = self.home_id();
        let attempts=self.reads().snapshot(move|sql|{
            let mut q=sql.prepare("SELECT r.run_id,r.attempt_id,r.project_id,r.step_id,r.generation,r.work_generation,a.request,r.unit_name,r.cgroup FROM runs r JOIN attempts a USING(attempt_id) WHERE a.phase<>'terminal'")?;
            let mut rows=q.query([])?;let mut out=vec![];
            while let Some(row)=rows.next()?{
                let run:RunId=calls::parse_id(row.get(0)?)?;let id=AttemptKey{home:home_id,run,attempt:calls::parse_id(row.get(1)?)?,project:row.get::<_,Option<String>>(2)?.map(calls::parse_id).transpose()?,step:row.get::<_,Option<String>>(3)?.map(calls::parse_id).transpose()?,generation:StepGeneration(row.get::<_,i64>(4)? as u64),work:WorkGeneration(row.get::<_,i64>(5)? as u64)};
                let raw:String=row.get(6)?;let frozen:Value=serde_json::from_str(&raw)?;let capability=serde_json::from_value(if id.step.is_some(){frozen["provenance"]["runtime"]["capability"].clone()}else{frozen["function"]["bundle"]["capability"].clone()})?;
                let guardian=stored_guardian(sql,&id,home_id)?;let cgroup:Option<String>=row.get(8)?;
                out.push(AdoptionAttempt{identity:id.clone(),guardian,run_dir:home.join("runs").join(run.to_string()),unit:row.get::<_,Option<String>>(7)?.unwrap_or_else(||sluice_process::systemd::TransientService::for_test(run).name().into()),service_cgroup:cgroup.map(|c|c.strip_suffix("/control").unwrap_or(&c).to_string()),capability});
            }Ok(out)
        }).await.map_err(|e|e.into_public(true))?;
        for attempt in attempts {
            let link = LocalLink {
                broker: self.clone(),
                capability: attempt.capability.clone(),
            };
            if let Err(e) = adopt_attempt(&attempt, &link, self.host()).await {
                tracing::warn!(run=%attempt.identity.run,error=%e,"adoption deferred");
            }
        }
        Ok(())
    }
    pub async fn serve(&self, stop: CancellationToken) -> Result<(), PublicError> {
        use std::os::unix::fs::PermissionsExt;
        let path = self.home().join("coordinator.sock");
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(storage(e)),
        }
        let listener = UnixListener::bind(&path).map_err(storage)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(storage)?;
        self.adopt().await?;
        let mut clients = JoinSet::new();
        let scheduler_stop = stop.child_token();
        let scheduler_broker = self.clone();
        let scheduler_task = tokio::spawn(scheduler::run(scheduler_broker, scheduler_stop));
        let permits = Arc::new(tokio::sync::Semaphore::new(64));
        loop {
            tokio::select! {
                _=stop.cancelled()=>break,
                Some(result)=clients.join_next()=>{if let Err(e)=result{tracing::error!(error=%e,"coordinator client task failed");}},
                accepted=listener.accept()=>{let (stream,_)=accepted.map_err(storage)?;let Ok(permit)=permits.clone().try_acquire_owned()else{drop(stream);continue;};let broker=self.clone();let stop=stop.child_token();clients.spawn(async move{let _permit=permit;if let Err(e)=broker.connection(stream,stop).await{tracing::debug!(error=%e,"socket client closed");}});}
            }
        }
        drop(listener);
        while clients.join_next().await.is_some() {}
        self.calls().close().await;
        scheduler_task.await.map_err(storage)??;
        std::fs::remove_file(path).map_err(storage)?;
        self.writer()
            .shutdown()
            .await
            .map_err(|e| e.into_public(false))?;
        Ok(())
    }
    async fn connection(
        &self,
        mut stream: UnixStream,
        stop: CancellationToken,
    ) -> Result<(), PublicError> {
        let request: Request<Value> =
            tokio::time::timeout(Duration::from_secs(5), socket::read_frame(&mut stream))
                .await
                .map_err(storage)?
                .map_err(storage)?;
        if request.protocol != rpc::PROTOCOL_VERSION {
            return Err(conflict("unsupported protocol"));
        }
        if request.command.get("runtime").is_some() {
            let command: RuntimeCommand =
                rpc::decode_json(&serde_json::to_vec(&request.command).map_err(storage)?)?;
            match command {
                RuntimeCommand::AcquireScheduler { owner } => {
                    let result = self.acquire_scheduler(owner.clone()).await;
                    let accepted = result.is_ok();
                    let response = Reply {
                        protocol: 1,
                        request_id: request.request_id,
                        result: result.map(|()| CommandReply::Ack),
                    };
                    if let Err(e) = write_reply(&mut stream, &response).await {
                        if accepted {
                            self.release_scheduler(owner).await?;
                        }
                        return Err(storage(e));
                    }
                    if accepted {
                        use tokio::io::AsyncReadExt;
                        let mut byte = [0; 1];
                        tokio::select! {_=stream.read(&mut byte)=>{},_=stop.cancelled()=>{}};
                        self.release_scheduler(owner).await?;
                    }
                }
                RuntimeCommand::Changes(cursor) => {
                    let result = self.changes(cursor).await;
                    write_reply(
                        &mut stream,
                        &Reply {
                            protocol: 1,
                            request_id: request.request_id,
                            result,
                        },
                    )
                    .await
                    .map_err(storage)?;
                }
            }
            return Ok(());
        }
        if request.command.get("method").is_some() {
            let command: CoordinatorCommand =
                rpc::decode_json(&serde_json::to_vec(&request.command).map_err(storage)?)?;
            let result = self
                .guardian(command, request.run_capability.as_ref())
                .await;
            write_reply(
                &mut stream,
                &Reply {
                    protocol: 1,
                    request_id: request.request_id,
                    result,
                },
            )
            .await
            .map_err(storage)?;
        } else {
            if request.run_capability.is_some() {
                return Err(conflict("use authenticated guardian callback"));
            }
            let command: CommandRequest =
                rpc::decode_json(&serde_json::to_vec(&request.command).map_err(storage)?)?;
            let reply = tokio::select! {
                result=self.command(command)=>result,
                _=stop.cancelled()=>return Ok(()),
            };
            let result = match reply {
                Ok(reply) => RpcResult::Ok(Box::new(reply)),
                Err(e) => RpcResult::Error(e),
            };
            write_reply(
                &mut stream,
                &RpcReply {
                    protocol: 1,
                    request_id: request.request_id,
                    result,
                },
            )
            .await
            .map_err(storage)?;
        }
        Ok(())
    }
}
struct LocalLink<H: ExecutionHost> {
    broker: Coordinator<H>,
    capability: RunCapability,
}
impl<H: ExecutionHost> CoordinatorLink for LocalLink<H> {
    async fn request(&self, command: CoordinatorCommand) -> Result<CoordinatorReply, PublicError> {
        self.broker.guardian(command, Some(&self.capability)).await
    }
}
impl<H: ExecutionHost> RuntimeApi for Coordinator<H> {
    async fn command(&self, request: CommandRequest) -> Result<CommandReply, PublicError> {
        Coordinator::command(self, request).await
    }
    async fn changes(&self, cursor: ChangeCursor) -> Result<ChangeBatch, PublicError> {
        self.reads()
            .snapshot(move |sql| {
                let mut events = vec![];
                for project in &cursor.projects {
                    events.extend(
                        records::read_records(
                            sql,
                            Some(*project),
                            &records::RecordFilter {
                                since: Some(cursor.after),
                                limit: 1000,
                                ..Default::default()
                            },
                        )?
                        .into_page()?
                        .records,
                    );
                }
                events.sort_by_key(|r| r.seq.0);
                events.truncate(1000);
                let after = events.last().map_or(cursor.after, |r| r.seq);
                Ok(ChangeBatch {
                    records: events,
                    cursor: ChangeCursor {
                        after,
                        projects: cursor.projects,
                    },
                })
            })
            .await
            .map_err(|e| e.into_public(true))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "runtime",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RuntimeCommand {
    AcquireScheduler { owner: String },
    Changes(ChangeCursor),
}
pub use crate::client::CoordinatorClient;

pub(crate) fn context(
    sql: &Connection,
    project: ProjectId,
    catalog: &Catalog,
) -> sluice_store::Result<PlanContext> {
    let (rev, doc): (i64, String) = sql.query_row(
        "SELECT rev,doc FROM plans WHERE project_id=?1",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let doc: JsonMap = serde_json::from_str(&doc)?;
    let plan = Plan::parse(&doc, catalog).map_err(|errors| PublicError::Invalid {
        message: "invalid stored plan".into(),
        errors: errors.into_iter().map(|e| e.to_string()).collect(),
    })?;
    Ok(PlanContext {
        project,
        revision: Revision(rev as u64),
        plan,
    })
}
fn resource_limits(
    sql: &Connection,
    project: ProjectId,
) -> sluice_store::Result<indexmap::IndexMap<String, sluice_model::plan::ResourceLimit>> {
    Ok(resources::declarations(sql, project)?
        .into_iter()
        .map(|(n, r)| {
            (
                n,
                match r.declaration {
                    resources::Capacity::Fixed(n) => sluice_model::plan::ResourceLimit::Fixed(n),
                    resources::Capacity::Function(_) => sluice_model::plan::ResourceLimit::Dynamic,
                },
            )
        })
        .collect())
}
fn messages_project(
    sql: &Connection,
    selector: &ProjectSelector,
) -> sluice_store::Result<ProjectId> {
    sluice_store::messages::resolve_project(sql, selector)
}
fn projects_identity(sql: &Connection, id: ProjectId) -> sluice_store::Result<ProjectIdentity> {
    let p = projects::resolve(sql, &ProjectSelector::Id(id))?;
    Ok(ProjectIdentity {
        project_id: id,
        name: p.name,
    })
}
fn edit_project(command: &CommandRequest) -> Option<ProjectSelector> {
    Some(match command {
        CommandRequest::PlanPatch(r) => r.project.clone(),
        CommandRequest::StepAdd(r) => r.project.clone(),
        CommandRequest::StepUpdate(r) => r.project.clone(),
        CommandRequest::StepRemove(r) => r.project.clone(),
        CommandRequest::StepPause(r) => r.project.clone(),
        CommandRequest::StepSetInput(r) => r.project.clone(),
        CommandRequest::EdgeAdd(r) | CommandRequest::EdgeRemove(r) => r.project.clone(),
        CommandRequest::UnitTag(r) => r.project.clone(),
        _ => return None,
    })
}
fn step_identity(id: &AttemptKey) -> sluice_store::Result<attempts::AttemptIdentity> {
    Ok(attempts::AttemptIdentity {
        project: id.project.ok_or_else(|| conflict("missing project"))?,
        step: id.step.clone().ok_or_else(|| conflict("missing step"))?,
        generation: id.generation,
        work: id.work,
        run: id.run,
        attempt: id.attempt,
    })
}
async fn write_reply<T: Serialize>(stream: &mut UnixStream, reply: &T) -> std::io::Result<()> {
    tokio::time::timeout(Duration::from_secs(5), socket::write_frame(stream, reply))
        .await
        .map_err(std::io::Error::other)?
}

fn require_invocation(
    sql: &Connection,
    id: &AttemptKey,
    invocation: InvocationId,
) -> sluice_store::Result<()> {
    let raw: String = sql.query_row(
        "SELECT request FROM attempts WHERE attempt_id=?1",
        [id.attempt.to_string()],
        |r| r.get(0),
    )?;
    let request: Value = serde_json::from_str(&raw)?;
    if !request["runtime_starts"]
        .as_array()
        .is_some_and(|starts| starts.iter().any(|s| s["invocation"] == json!(invocation)))
    {
        return Err(conflict("delivery acknowledgement has no admitted invocation").into());
    }
    Ok(())
}

fn guardian_key(command: &CoordinatorCommand) -> &AttemptKey {
    match command {
        CoordinatorCommand::Claim(g) => &g.identity,
        CoordinatorCommand::Started { identity, .. }
        | CoordinatorCommand::Messages { identity, .. }
        | CoordinatorCommand::DeliverAck { identity, .. }
        | CoordinatorCommand::Callback { identity, .. } => identity,
        CoordinatorCommand::CancelIntent(id) | CoordinatorCommand::Submissions(id) => id,
        CoordinatorCommand::Complete(j) => &j.identity,
    }
}
fn store_guardian(g: &GuardianIdentity) -> attempts::GuardianIdentity {
    attempts::GuardianIdentity {
        unit_name: g.unit.clone(),
        boot_id: g.process.boot_id.clone(),
        pid: g.process.pid,
        start: g.process.start_time.to_string(),
        cgroup: g.process.cgroup.clone(),
        socket_challenge: g.socket_challenge.clone(),
    }
}
fn persist_guardian(
    tx: &mut sluice_store::WriteTransaction<'_>,
    id: &AttemptKey,
    g: attempts::GuardianIdentity,
) -> sluice_store::Result<()> {
    tx.sql().execute("UPDATE runs SET unit_name=?2,boot_id=?3,guardian_pid=?4,guardian_start=?5,cgroup=?6,socket_challenge=?7 WHERE run_id=?1",(id.run.to_string(),g.unit_name,g.boot_id,g.pid,g.start,g.cgroup,g.socket_challenge))?;
    Ok(())
}
fn stored_guardian(
    sql: &Connection,
    id: &AttemptKey,
    _home: HomeId,
) -> sluice_store::Result<Option<GuardianIdentity>> {
    let row:Option<(String,String,u32,String,String,String)>=sql.query_row("SELECT unit_name,boot_id,guardian_pid,guardian_start,cgroup,socket_challenge FROM runs WHERE run_id=?1 AND guardian_pid IS NOT NULL",[id.run.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    row.map(|(unit, boot_id, pid, start, cgroup, socket_challenge)| {
        Ok(GuardianIdentity {
            identity: id.clone(),
            process: ProcessIdentity {
                pid,
                start_time: start
                    .parse()
                    .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?,
                boot_id,
                cgroup,
            },
            unit,
            socket_challenge,
        })
    })
    .transpose()
}
fn completion_kind(result: PayloadResult) -> (attempts::CompletionKind, JsonMap) {
    use attempts::CompletionKind as K;
    match result {
        PayloadResult::Succeeded(outputs) => (K::Succeeded, outputs),
        PayloadResult::Failed(e) => (K::Failed(e), JsonMap::default()),
        PayloadResult::Rejected(message) => (K::Rejected { message }, JsonMap::default()),
        PayloadResult::Cancelled(message) => (K::Cancelled { message }, JsonMap::default()),
        PayloadResult::Lost(message) => (K::Lost { message }, JsonMap::default()),
        PayloadResult::Unknown(message) => (K::Unknown { message }, JsonMap::default()),
    }
}

fn run_project(sql: &Connection, run: RunId) -> sluice_store::Result<ProjectId> {
    let id: String = sql.query_row(
        "SELECT project_id FROM runs WHERE run_id=?1",
        [run.to_string()],
        |r| r.get(0),
    )?;
    calls::parse_id(id)
}
