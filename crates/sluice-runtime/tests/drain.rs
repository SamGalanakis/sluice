use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    commands::FnCall,
    error::PublicError,
    ids::{AttemptId, ProjectId, ProjectSelector, RunId},
    plan::FnSignature,
    rpc::JsonMap,
    types::Type,
};
use sluice_runtime::{calls::*, drain};
use sluice_store::{ReadPool, RetrySafety, Writer};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("sluice-test-p3-04-{}", RunId::new()));
        let path = sluice_process::host::guard_scratch_home(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn setup() -> (Home, Writer, ReadPool) {
    let h = Home::new();
    let w = Writer::open(&h.0).unwrap();
    let r = ReadPool::open(&h.0, 2).unwrap();
    (h, w, r)
}
async fn project(w: &Writer) -> ProjectId {
    let p = ProjectId::new();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "projects");
        Ok(())
    })
    .await
    .unwrap();
    p
}
fn inputs(v: Value) -> JsonMap {
    serde_json::from_value(v).unwrap()
}
fn request(project: Option<ProjectId>, direct: bool) -> FnCall {
    FnCall {
        name: "test.echo".into(),
        inputs: inputs(json!({"value":"hello"})),
        project: project.map(ProjectSelector::Id),
        wait_seconds: Some(0),
        direct,
        author: Some("test".into()),
    }
}
#[derive(Default)]
struct Registry {
    needs: bool,
}
impl CallRegistry for Registry {
    fn freeze(
        &self,
        _project: Option<ProjectId>,
        name: &str,
    ) -> Result<FrozenFunction, PublicError> {
        if name != "test.echo" && name != "test.capacity" {
            return Err(PublicError::NotFound {
                message: format!("no fn {name}"),
            });
        }
        let mut f = FrozenFunction::from_signature(
            name.into(),
            FnSignature {
                inputs: if name == "test.echo" {
                    IndexMap::from([
                        ("value".into(), Type::String),
                        ("maybe".into(), Type::Optional(Box::new(Type::String))),
                    ])
                } else {
                    IndexMap::new()
                },
                outputs: if name == "test.echo" {
                    IndexMap::from([("value".into(), Type::String)])
                } else {
                    IndexMap::from([("capacity".into(), Type::Int)])
                },
                ..FnSignature::default()
            },
            "test-release".into(),
        );
        if self.needs && name == "test.echo" {
            f.needs.insert("lane".into(), 1);
        }
        Ok(f)
    }
}
#[derive(Default)]
struct Guardian {
    admissions: Mutex<Vec<AdmittedCall>>,
    wake: tokio::sync::Notify,
}
impl CallGuardian for Guardian {
    async fn launch(&self, call: AdmittedCall) -> Result<(), PublicError> {
        self.admissions.lock().unwrap().push(call);
        self.wake.notify_one();
        Ok(())
    }
}
impl Guardian {
    async fn get(&self) -> AdmittedCall {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let notified = self.wake.notified();
                if let Some(a) = self.admissions.lock().unwrap().pop() {
                    return a;
                }
                notified.await;
            }
        })
        .await
        .unwrap()
    }
}
fn service(w: &Writer, r: &ReadPool, g: Arc<Guardian>) -> Calls<Registry, Guardian> {
    Calls::new(w.clone(), r.clone(), Arc::new(Registry::default()), g)
}
fn completion(a: &AdmittedCall) -> CallCompletion {
    CallCompletion {
        call: a.call,
        attempt: a.attempt,
        project: a.project,
        completion_id: format!("done/{}", a.call),
        outputs: a.inputs.clone(),
        error: None,
        processes_gone: true,
    }
}

#[tokio::test]
async fn drain_ownership_is_durable_and_release_preserves_preexisting_pause() {
    let (_h, w, r) = setup().await;
    let p = project(&w).await;
    let q = ProjectId::new();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE projects SET paused=1 WHERE project_id=?1",
            [p.to_string()],
        )?;
        tx.sql().execute(
            "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'q','now')",
            [q.to_string()],
        )?;
        tx.changed(None, "projects");
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        drain::drain(&w, None, "owner".into()).await.unwrap(),
        vec![q]
    );
    assert!(
        drain::drain(&w, None, "owner".into())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        drain::drain(&w, None, "other".into()).await,
        Err(PublicError::Conflict { .. })
    ));
    let status = drain::status(&r).await.unwrap();
    assert_eq!(status.owner.as_deref(), Some("owner"));
    assert!(status.drained);
    assert_eq!(
        drain::release(&w, "recovery".into()).await.unwrap(),
        vec![q]
    );
    let paused = r
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT paused FROM projects WHERE project_id=?1",
                [p.to_string()],
                |r| r.get::<_, bool>(0),
            )?)
        })
        .await
        .unwrap();
    assert!(paused);
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn drain_survives_writer_restart_and_home_calls_hold_it() {
    let (h, w, r) = setup().await;
    let g = Arc::new(Guardian::default());
    let s = service(&w, &r, g.clone());
    s.fn_call(request(None, true)).await.unwrap();
    let a = g.get().await;
    drain::drain(&w, None, "owner".into()).await.unwrap();
    s.close().await;
    w.shutdown().await.unwrap();
    drop(s);
    drop(w);
    let fresh = Writer::open(&h.0).unwrap();
    let status = drain::status(&r).await.unwrap();
    assert!(!status.drained);
    assert_eq!(status.blockers[0].identity, a.call.to_string());
    assert_eq!(status.mode, "drain");
    assert!(complete(&fresh, completion(&a)).await.unwrap());
    assert!(drain::status(&r).await.unwrap().drained);
    assert_eq!(drain::status(&r).await.unwrap().mode, "drain");
    drain::release(&fresh, "owner".into()).await.unwrap();
    fresh.shutdown().await.unwrap();
}
#[tokio::test]
async fn zero_capacity_drain_growth_grants_admitted_section_then_completion_stops_observations() {
    let (_h, w, r) = setup().await;
    let p = project(&w).await;
    // Real project step reservation projection, so store lease functions enforce its identity.
    let run = RunId::new();
    let attempt = AttemptId::new();
    w.write(RetrySafety::NonIdempotent,move|tx|{
        tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status) VALUES (?1,'work',0,'{}','running')",[p.to_string()])?;
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,'work','executing','{}','hash','now')",(attempt.to_string(),p.to_string()))?;
        tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at) VALUES (?1,?2,?3,'work','now')",(run.to_string(),p.to_string(),attempt.to_string()))?;
        tx.sql().execute("INSERT INTO resources(scope,project_id,name,declaration,observed_capacity) VALUES (?1,?1,'lane',?2,0)",(p.to_string(),json!({"capacity_fn":"test.capacity"}).to_string()))?;
        tx.changed(Some(p),"status");Ok(())
    }).await.unwrap();
    let lease = w
        .write(RetrySafety::NonIdempotent, move |tx| {
            sluice_store::resources::request_lease_keyed(tx, run, "lane", 1, "work/section")
        })
        .await
        .unwrap();
    drain::drain(&w, None, "owner".into()).await.unwrap();
    assert_eq!(drain::observations(&r, p).await.unwrap().len(), 1);
    let status = drain::status(&r).await.unwrap();
    assert!(!status.drained);
    assert!(
        status
            .blockers
            .iter()
            .any(|b| b.resource.as_deref() == Some("lane"))
    );
    let g = Arc::new(Guardian::default());
    let s = service(&w, &r, g.clone());
    assert!(s.fn_call(request(Some(p), true)).await.is_err());
    assert!(s.capacity_call(p, "lane".into()).await.unwrap().is_some());
    let observation = g.get().await;
    assert_eq!(observation.function.timeout_seconds, Some(10));
    assert_eq!(s.capacity_call(p, "lane".into()).await.unwrap(), None);
    let mut done = completion(&observation);
    done.outputs = inputs(json!({"capacity":2}));
    complete(&w, done).await.unwrap();
    let granted = w
        .write(RetrySafety::NonIdempotent, move |tx| {
            sluice_store::resources::grant_leases(tx, p)
        })
        .await
        .unwrap();
    assert!(granted.contains(&lease));
    w.write(RetrySafety::Idempotent, move |tx| {
        drain::ensure_admission(tx, &drain::Admission::Callback)?;
        drain::ensure_admission(tx, &drain::Admission::SectionLease)?;
        sluice_store::resources::release_lease(tx, lease, run)?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(drain::observations(&r, p).await.unwrap().is_empty());
    assert!(s.capacity_call(p, "lane".into()).await.is_err());
    w.write(RetrySafety::Idempotent, move |tx| {
        tx.sql().execute(
            "UPDATE attempts SET phase='terminal',finished_at='now' WHERE attempt_id=?1",
            [attempt.to_string()],
        )?;
        tx.sql().execute(
            "UPDATE runs SET finished_at='now' WHERE run_id=?1",
            [run.to_string()],
        )?;
        tx.changed(Some(p), "status");
        Ok(())
    })
    .await
    .unwrap();
    assert!(drain::status(&r).await.unwrap().drained);
    s.close().await;
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn pending_calls_are_reported_without_relaunching() {
    let (_h, w, r) = setup().await;
    let p = project(&w).await;
    let g = Arc::new(Guardian::default());
    let s = service(&w, &r, g);
    let c = s.fn_call(request(Some(p), false)).await.unwrap();
    drain::drain(&w, None, "owner".into()).await.unwrap();
    let status = drain::status(&r).await.unwrap();
    assert_eq!(status.pending_calls, vec![c.call.to_string()]);
    assert!(status.drained);
    assert!(s.admit_queued(c.call, Some(p)).await.is_err());
    assert!(
        drain::check_command(
            &r,
            &sluice_model::commands::CommandRequest::FnCall(request(None, false))
        )
        .await
        .is_err()
    );
    assert!(
        drain::check_command(&r, &sluice_model::commands::CommandRequest::ProjectsList)
            .await
            .is_ok()
    );
    s.close().await;
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn unknown_project_drain_rolls_back_the_home_fence() {
    let (_h, w, r) = setup().await;
    assert!(
        drain::drain(
            &w,
            Some(vec![ProjectSelector::Id(ProjectId::new())]),
            "owner".into()
        )
        .await
        .is_err()
    );
    assert_eq!(drain::status(&r).await.unwrap().mode, "normal");
    w.shutdown().await.unwrap();
}
