//! Durable maintenance fencing. Owner disappearance never releases the fence.
use crate::calls::{busy, parse_id, public, timestamp};
use serde::Serialize;
use sluice_model::{
    commands::CommandRequest,
    error::PublicError,
    events::Event,
    ids::{ProjectId, ProjectSelector},
};
use sluice_store::{ReadPool, RetrySafety, WriteTransaction, Writer, messages, resources};

#[derive(Debug, Clone)]
pub enum Admission {
    Plan,
    UserCall,
    Capacity {
        project: ProjectId,
        resource: String,
    },
    Callback,
    SectionLease,
}
/// Compose this check in the same transaction that reserves work.
pub fn ensure_admission(
    tx: &WriteTransaction<'_>,
    admission: &Admission,
) -> sluice_store::Result<()> {
    let mode: String =
        tx.sql()
            .query_row("SELECT mode FROM maintenance WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
    if matches!(admission, Admission::Callback | Admission::SectionLease) {
        return Ok(());
    }
    if mode == "normal" {
        return Ok(());
    }
    if mode == "drain"
        && let Admission::Capacity { project, resource } = admission
    {
        let needed:bool=tx.sql().query_row("SELECT EXISTS(SELECT 1 FROM leases l JOIN runs r USING(run_id) JOIN attempts a USING(attempt_id) WHERE l.project_id=?1 AND l.resource=?2 AND l.state IN ('waiting','held') AND r.finished_at IS NULL AND a.phase<>'terminal')",(project.to_string(),resource),|r|r.get(0))?;
        if needed {
            return Ok(());
        }
    }
    Err(busy(format!("{mode} rejects new work")).into())
}
/// Maintenance commands can edit/retry while cutover holds admission closed.
pub fn ensure_edit(tx: &WriteTransaction<'_>) -> sluice_store::Result<()> {
    let mode: String =
        tx.sql()
            .query_row("SELECT mode FROM maintenance WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
    if mode == "cutover" {
        Ok(())
    } else {
        ensure_admission(tx, &Admission::Plan)
    }
}
/// Register before the dispatch table. Resource observations use ensure_admission
/// with a coordinator-created Capacity request, never user-provided privilege.
pub async fn check_command(reads: &ReadPool, request: &CommandRequest) -> Result<(), PublicError> {
    let fenced = matches!(
        request,
        CommandRequest::FnCall(_)
            | CommandRequest::PlanPatch(_)
            | CommandRequest::StepAdd(_)
            | CommandRequest::UnitAdd(_)
            | CommandRequest::EdgeAdd(_)
            | CommandRequest::EdgeRemove(_)
            | CommandRequest::StepUpdate(_)
            | CommandRequest::StepRemove(_)
            | CommandRequest::UnitTag(_)
            | CommandRequest::StepPause(_)
            | CommandRequest::PlanPrune(_)
            | CommandRequest::PlanSetInput(_)
            | CommandRequest::StepSetInput(_)
            | CommandRequest::StepRetry(_)
    );
    if !fenced {
        return Ok(());
    }
    let user_call = matches!(request, CommandRequest::FnCall(_));
    reads
        .snapshot(move |sql| {
            let mode: String =
                sql.query_row("SELECT mode FROM maintenance WHERE singleton=1", [], |r| {
                    r.get(0)
                })?;
            if mode != "normal" && (mode != "cutover" || user_call) {
                return Err(busy(format!("{mode} rejects new plan work and user calls")).into());
            }
            Ok(())
        })
        .await
        .map_err(public)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DrainBlocker {
    pub kind: String,
    pub identity: String,
    pub project_id: Option<ProjectId>,
    pub resource: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct DrainStatus {
    pub mode: String,
    pub owner: Option<String>,
    pub paused: Vec<ProjectId>,
    pub blockers: Vec<DrainBlocker>,
    pub pending_calls: Vec<String>,
    pub open_questions: usize,
    pub drained: bool,
}
/// Home admission is fenced even when only selected projects are paused.
pub async fn drain(
    writer: &Writer,
    projects: Option<Vec<ProjectSelector>>,
    author: String,
) -> Result<Vec<ProjectId>, PublicError> {
    if author.trim().is_empty() {
        return Err(PublicError::BadRequest {
            message: "drain owner must not be blank".into(),
        });
    }
    writer.write(RetrySafety::NonIdempotent,move|tx|{
        let (mode,owner,ledger):(String,Option<String>,String)=tx.sql().query_row("SELECT mode,owner,paused_projects FROM maintenance WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if mode=="cutover"{return Err(busy("cutover fence cannot be drained").into());}
        if mode=="drain" && owner.as_deref()!=Some(&author){return Err(PublicError::Conflict{message:format!("drain belongs to {}",owner.as_deref().unwrap_or("unknown")),current_rev:None}.into());}
        let targets=if let Some(selectors)=projects.filter(|s|!s.is_empty()){
            selectors.iter().map(|s|messages::resolve_project(tx.sql(),s)).collect::<sluice_store::Result<Vec<_>>>()?
        }else{
            let mut stmt=tx.sql().prepare("SELECT project_id FROM projects WHERE deleted_at IS NULL AND archived=0 ORDER BY name")?;
            let rows=stmt.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            rows.into_iter().map(parse_id).collect::<sluice_store::Result<Vec<_>>>()?
        };
        let mut ledger:Vec<ProjectId>=serde_json::from_str(&ledger)?;
        let mut added=vec![];
        for project in targets{
            let paused:bool=tx.sql().query_row("SELECT paused FROM projects WHERE project_id=?1",[project.to_string()],|r|r.get(0))?;
            if paused{continue;}
            let at=timestamp(tx)?;
            tx.sql().execute("UPDATE projects SET paused=1,settings_rev=settings_rev+1,changed_at=?2 WHERE project_id=?1",(project.to_string(),at))?;
            tx.append_record(Some(project),Event::ProjectPause{paused:true,reason:Some("drain: paused for maintenance".into()),author:author.clone()})?;
            tx.changed(Some(project),"projects");tx.changed(Some(project),"status");
            if !ledger.contains(&project){ledger.push(project);}added.push(project);
        }
        let at=timestamp(tx)?;
        tx.sql().execute("UPDATE maintenance SET mode='drain',owner=?1,paused_projects=?2,revision=revision+1,changed_at=?3 WHERE singleton=1",(author,serde_json::to_string(&ledger)?,at))?;
        tx.changed(None,"maintenance");Ok(added)
    }).await
}
/// Explicit release may recover an absent owner's drain. Cutover is a distinct fence.
pub async fn release(writer: &Writer, author: String) -> Result<Vec<ProjectId>, PublicError> {
    writer.write(RetrySafety::NonIdempotent,move|tx|{
        let (mode,ledger):(String,String)=tx.sql().query_row("SELECT mode,paused_projects FROM maintenance WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if mode=="cutover"{return Err(busy("release cannot clear the cutover fence").into());}
        if mode=="normal"{return Ok(vec![]);}
        let ledger:Vec<ProjectId>=serde_json::from_str(&ledger)?;
        let mut released=vec![];
        for project in ledger{
            let at=timestamp(tx)?;
            let n=tx.sql().execute("UPDATE projects SET paused=0,settings_rev=settings_rev+1,changed_at=?2 WHERE project_id=?1 AND paused=1 AND deleted_at IS NULL",(project.to_string(),at))?;
            if n>0{
                tx.append_record(Some(project),Event::ProjectPause{paused:false,reason:Some("drain released".into()),author:author.clone()})?;
                tx.changed(Some(project),"projects");tx.changed(Some(project),"status");released.push(project);
            }
        }
        let at=timestamp(tx)?;
        tx.sql().execute("UPDATE maintenance SET mode='normal',owner=NULL,paused_projects='[]',revision=revision+1,changed_at=?1 WHERE singleton=1",[at])?;
        tx.changed(None,"maintenance");Ok(released)
    }).await
}
pub async fn status(reads: &ReadPool) -> Result<DrainStatus, PublicError> {
    reads.snapshot(|sql|{
        let (mode,owner,paused):(String,Option<String>,String)=sql.query_row("SELECT mode,owner,paused_projects FROM maintenance WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let mut blockers=vec![];
        let mut stmt=sql.prepare("SELECT a.attempt_id,a.project_id,r.run_id,json_extract(a.request,'$.capacity.resource') FROM attempts a LEFT JOIN runs r USING(attempt_id) WHERE a.phase<>'terminal' OR (r.run_id IS NOT NULL AND r.finished_at IS NULL)")?;
        for row in stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?)))?{
            let (attempt,p,run,resource)=row?;
            blockers.push(DrainBlocker{kind:if resource.is_some(){"observation"}else{"guardian"}.into(),identity:run.unwrap_or(attempt),project_id:p.map(parse_id).transpose()?,resource});
        }
        let mut stmt=sql.prepare("SELECT lease_id,project_id,resource,state FROM leases WHERE state IN ('waiting','held') ORDER BY lease_id")?;
        for row in stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?{
            let (id,p,resource,state)=row?;
            blockers.push(DrainBlocker{kind:format!("lease.{state}"),identity:id.to_string(),project_id:p.map(parse_id).transpose()?,resource:Some(resource)});
        }
        let mut stmt=sql.prepare("SELECT call_id FROM calls WHERE status='pending' ORDER BY created_at,call_id")?;
        let pending_calls=stmt.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let questions:i64=sql.query_row("SELECT count(*) FROM questions WHERE state='open'",[],|r|r.get(0))?;
        Ok(DrainStatus{drained:mode=="drain"&&blockers.is_empty(),mode,owner,paused:serde_json::from_str(&paused)?,blockers,pending_calls,open_questions:questions as usize})
    }).await.map_err(public)
}
/// Scheduler capacity timer consumes this list and applies the bounded guardian pipeline.
pub async fn observations(
    reads: &ReadPool,
    project: ProjectId,
) -> Result<Vec<resources::Resource>, PublicError> {
    reads.snapshot(move|sql|{
        let observations=resources::capacity_observations(sql,project)?;
        let mode:String=sql.query_row("SELECT mode FROM maintenance WHERE singleton=1",[],|r|r.get(0))?;
        if mode!="drain"{return Ok(observations);}
        let mut needed=vec![];
        for resource in observations{
            let live:bool=sql.query_row("SELECT EXISTS(SELECT 1 FROM leases l JOIN runs r USING(run_id) JOIN attempts a USING(attempt_id) WHERE l.project_id=?1 AND l.resource=?2 AND l.state IN ('waiting','held') AND r.finished_at IS NULL AND a.phase<>'terminal')",(project.to_string(),&resource.name),|r|r.get(0))?;
            if live{needed.push(resource);}
        }Ok(needed)
    }).await.map_err(public)
}

/// Notification-driven maintenance wait; returns blockers on a bounded timeout.
pub async fn wait(
    writer: &Writer,
    reads: &ReadPool,
    timeout: std::time::Duration,
) -> Result<DrainStatus, PublicError> {
    use sluice_store::ChangeKey;
    let mut receiver = writer.subscribe();
    let mut subscription = reads
        .subscribe(
            writer,
            vec![
                ChangeKey::new(None, "maintenance"),
                ChangeKey::new(None, "log"),
            ],
        )
        .await
        .map_err(public)?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let state = status(reads).await?;
        if state.drained || state.mode != "drain" || tokio::time::Instant::now() >= deadline {
            return Ok(state);
        }
        // Lease release may invalidate resources without appending a home record.
        // A coalesced writer notification always causes a fresh durable status.
        tokio::select! {
            _=tokio::time::sleep_until(deadline)=>return status(reads).await,
            changed=receiver.changed()=>if changed.is_err(){return Err(PublicError::Storage{message:"drain writer closed".into()});},
            changed=subscription.wait()=>{changed.map_err(public)?;},
        }
    }
}
