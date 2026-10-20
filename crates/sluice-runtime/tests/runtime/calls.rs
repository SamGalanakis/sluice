use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    commands::{FnCall, StepStatus},
    error::PublicError,
    ids::{AttemptId, ProjectId, ProjectSelector, RunId},
    plan::FnSignature,
    rpc::JsonMap,
    types::Type,
};
use sluice_runtime::{calls::*, drain};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    records::{self, RecordFilter},
};
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
    std::fs::write(
        h.0.join("config.json"),
        json!({"execution":{
            "defaults":{"memory":"64MiB","cpus":0.1,"tasks":64},
            "guardian_memory":"64MiB"
        }})
        .to_string(),
    )
    .unwrap();
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
        limits: None,
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
// This guardian only records launches: an accepted completion also proves its
// service gone. Mirror the coordinator's separate release after terminalization.
async fn complete(writer: &Writer, completion: CallCompletion) -> Result<bool, PublicError> {
    let attempt = completion.attempt;
    let accepted = sluice_runtime::calls::complete(writer, completion).await?;
    if accepted {
        writer
            .write(RetrySafety::Idempotent, move |tx| {
                sluice_store::resources::release_service(tx, attempt)
            })
            .await?;
    }
    Ok(accepted)
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

async fn queued_home_call_has_durable_inputs_and_monotonic_status_records(
    w: &Writer,
    r: &ReadPool,
) {
    let g = Arc::new(Guardian::default());
    let service = service(w, r, g.clone());
    let pending = service.fn_call(request(None, false)).await.unwrap();
    assert_eq!(pending.status, StepStatus::Pending);
    assert_eq!(pending.inputs, inputs(json!({"value":"hello"})));
    assert_eq!(service.queued().await.unwrap().len(), 1);
    assert!(service.admit_queued(pending.call, None).await.unwrap());
    let admitted = g.get().await;
    assert!(!service.admit_queued(pending.call, None).await.unwrap());
    assert_eq!(
        call_status(r, pending.call, None).await.unwrap().status,
        StepStatus::Running
    );
    assert!(
        claim(
            w,
            admitted.clone(),
            sluice_store::attempts::GuardianIdentity {
                unit_name: format!("sluice-test-{}", admitted.call),
                boot_id: "test".into(),
                pid: 1,
                start: "1".into(),
                cgroup: "/test".into(),
                socket_challenge: "test".into()
            }
        )
        .await
        .unwrap()
    );
    assert!(started(w, admitted.call, admitted.attempt).await.unwrap());
    assert!(complete(w, completion(&admitted)).await.unwrap());
    assert_eq!(
        call_status(r, pending.call, None).await.unwrap().status,
        StepStatus::Succeeded
    );
    let page = r
        .snapshot(|sql| {
            records::read_records(
                sql,
                None,
                &RecordFilter {
                    since: Some(sluice_model::ids::RecordSeq(0)),
                    ..RecordFilter::default()
                },
            )?
            .into_page()
        })
        .await
        .unwrap();
    assert_eq!(page.records.len(), 3);
    assert!(page.records.windows(2).all(|w| w[0].seq.0 < w[1].seq.0));
    service.close().await;
}
async fn project_scope_and_inputs_are_checked_before_writing(w: &Writer, r: &ReadPool) {
    let p = project(w).await;
    let g = Arc::new(Guardian::default());
    let s = service(w, r, g);
    let mut bad = request(Some(p), false);
    bad.inputs = inputs(json!({"value":3,"extra":true}));
    assert!(matches!(s.fn_call(bad).await,Err(PublicError::Invalid{errors,..})if errors.len()==2));
    let mut missing = request(None, false);
    missing.name = "missing".into();
    assert!(matches!(
        s.fn_call(missing).await,
        Err(PublicError::NotFound { .. })
    ));
    let mut external = request(None, false);
    external.name = "core.external".into();
    assert!(s.fn_call(external).await.is_err());
    assert!(s.queued().await.unwrap().is_empty());
    let c = s.fn_call(request(Some(p), false)).await.unwrap();
    assert!(matches!(
        call_status(r, c.call, None).await,
        Err(PublicError::NotFound { .. })
    ));
    s.close().await;
}

async fn wait_timeout_and_completion_notifications_are_bounded(w: &Writer, r: &ReadPool) {
    let g = Arc::new(Guardian::default());
    let s = Arc::new(service(w, r, g.clone()));
    let c = s.fn_call(request(None, true)).await.unwrap();
    let a = g.get().await;
    let start = tokio::time::Instant::now();
    let running = s
        .wait(c.call, None, Duration::from_millis(25))
        .await
        .unwrap();
    assert_eq!(running.status, StepStatus::Running);
    assert!(start.elapsed() < Duration::from_secs(1));
    let wait_s = s.clone();
    let wait = tokio::spawn(async move { wait_s.wait(c.call, None, Duration::from_secs(2)).await });
    complete(w, completion(&a)).await.unwrap();
    assert_eq!(wait.await.unwrap().unwrap().status, StepStatus::Succeeded);
    s.close().await;
}
async fn terminal_callbacks_are_fenced_and_require_cleanup_and_frozen_outputs(
    w: &Writer,
    r: &ReadPool,
) {
    let g = Arc::new(Guardian::default());
    let s = service(w, r, g.clone());
    s.fn_call(request(None, true)).await.unwrap();
    let a = g.get().await;
    let mut no_cleanup = completion(&a);
    no_cleanup.processes_gone = false;
    assert!(complete(w, no_cleanup).await.is_err());
    let mut wrong = completion(&a);
    wrong.attempt = AttemptId::new();
    assert!(!complete(w, wrong).await.unwrap());
    let mut bad = completion(&a);
    bad.outputs = inputs(json!({"value":3}));
    assert!(complete(w, bad.clone()).await.unwrap());
    let status = call_status(r, a.call, None).await.unwrap();
    assert_eq!(status.status, StepStatus::Failed);
    assert!(matches!(status.error, Some(PublicError::Invalid { .. })));
    assert!(complete(w, bad).await.unwrap());
    assert!(
        !complete(
            w,
            CallCompletion {
                completion_id: "other".into(),
                ..completion(&a)
            }
        )
        .await
        .unwrap()
    );
    assert_eq!(
        call_status(r, a.call, None).await.unwrap().status,
        StepStatus::Failed
    );
    s.close().await;
}
#[tokio::test]
async fn capacity_blocks_direct_admission_atomically_and_drain_blocks_both_modes() {
    let (_h, w, r) = setup().await;
    let p = project(&w).await;
    w.write(RetrySafety::NonIdempotent,move|tx|{tx.sql().execute("INSERT INTO resources(scope,project_id,name,declaration,capacity) VALUES (?1,?1,'lane','0',0)",[p.to_string()])?;tx.changed(Some(p),"resources");Ok(())}).await.unwrap();
    let g = Arc::new(Guardian::default());
    let s = Calls::new(w.clone(), r.clone(), Arc::new(Registry { needs: true }), g);
    assert!(matches!(
        s.fn_call(request(Some(p), true)).await,
        Err(PublicError::Busy { .. })
    ));
    assert!(s.queued().await.unwrap().is_empty());
    let queued = s.fn_call(request(Some(p), false)).await.unwrap();
    assert!(s.admit_queued(queued.call, Some(p)).await.is_err());
    drain::drain(&w, None, "owner".into()).await.unwrap();
    assert!(s.fn_call(request(Some(p), false)).await.is_err());
    assert!(s.fn_call(request(None, true)).await.is_err());
    assert!(s.admit_queued(queued.call, Some(p)).await.is_err());
    s.close().await;
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn feed_trim_and_explicit_retention_preserve_live_and_referenced_calls() {
    let (_h, w, r) = setup().await;
    let g = Arc::new(Guardian::default());
    let s = service(&w, &r, g.clone());
    let pending = s.fn_call(request(None, false)).await.unwrap();
    s.fn_call(request(None, true)).await.unwrap();
    let a = g.get().await;
    complete(&w, completion(&a)).await.unwrap();
    w.write(RetrySafety::Idempotent, move |tx| {
        tx.sql().execute(
            "UPDATE calls SET finished_at='2000-01-01T00:00:00Z' WHERE call_id=?1",
            [a.call.to_string()],
        )?;
        tx.changed(None, "calls");
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(retain_calls(&w).await.unwrap(), 0);
    w.write(RetrySafety::NonIdempotent, |tx| {
        records::trim_to(tx, None, 1, 1)?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(retain_calls(&w).await.unwrap(), 0);
    w.write(RetrySafety::NonIdempotent, |tx| {
        tx.append_record(
            None,
            sluice_model::events::Event::RunOrphan { run: RunId::new() },
        )?;
        records::trim_to(tx, None, 1, 1)?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(retain_calls(&w).await.unwrap(), 1);
    assert_eq!(
        call_status(&r, pending.call, None).await.unwrap().status,
        StepStatus::Pending
    );
    s.close().await;
    w.shutdown().await.unwrap();
}

#[tokio::test]
async fn call_admission_waits_and_terminal_fencing() {
    let (_h, w, r) = setup().await;
    queued_home_call_has_durable_inputs_and_monotonic_status_records(&w, &r).await;
    project_scope_and_inputs_are_checked_before_writing(&w, &r).await;
    wait_timeout_and_completion_notifications_are_bounded(&w, &r).await;
    terminal_callbacks_are_fenced_and_require_cleanup_and_frozen_outputs(&w, &r).await;
    w.shutdown().await.unwrap();
}
