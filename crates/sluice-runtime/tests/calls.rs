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
async fn queued_home_call_has_durable_inputs_and_monotonic_status_records() {
    let (_h, w, r) = setup().await;
    let g = Arc::new(Guardian::default());
    let service = service(&w, &r, g.clone());
    let pending = service.fn_call(request(None, false)).await.unwrap();
    assert_eq!(pending.status, StepStatus::Pending);
    assert_eq!(pending.inputs, inputs(json!({"value":"hello"})));
    assert_eq!(service.queued().await.unwrap().len(), 1);
    assert!(service.admit_queued(pending.call, None).await.unwrap());
    let admitted = g.get().await;
    assert!(!service.admit_queued(pending.call, None).await.unwrap());
    assert_eq!(
        call_status(&r, pending.call, None).await.unwrap().status,
        StepStatus::Running
    );
    assert!(
        claim(
            &w,
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
    assert!(started(&w, admitted.call, admitted.attempt).await.unwrap());
    assert!(complete(&w, completion(&admitted)).await.unwrap());
    assert_eq!(
        call_status(&r, pending.call, None).await.unwrap().status,
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
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn project_scope_and_inputs_are_checked_before_writing() {
    let (_h, w, r) = setup().await;
    let p = project(&w).await;
    let g = Arc::new(Guardian::default());
    let s = service(&w, &r, g);
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
        call_status(&r, c.call, None).await,
        Err(PublicError::NotFound { .. })
    ));
    s.close().await;
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn direct_calls_skip_queue_and_waiter_abort_does_not_cancel_handoff() {
    let (_h, w, r) = setup().await;
    let g = Arc::new(Guardian::default());
    let s = Arc::new(service(&w, &r, g.clone()));
    let task_s = s.clone();
    let mut req = request(None, true);
    req.wait_seconds = Some(3600);
    let waiter = tokio::spawn(async move { task_s.fn_call(req).await });
    let a = g.get().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(s.queued().await.unwrap().is_empty());
    assert_eq!(
        call_status(&r, a.call, None).await.unwrap().status,
        StepStatus::Running
    );
    complete(&w, completion(&a)).await.unwrap();
    assert_eq!(
        call_status(&r, a.call, None).await.unwrap().outputs,
        Some(a.inputs.clone())
    );
    s.close().await;
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn wait_timeout_and_completion_notifications_are_bounded() {
    let (_h, w, r) = setup().await;
    let g = Arc::new(Guardian::default());
    let s = Arc::new(service(&w, &r, g.clone()));
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
    complete(&w, completion(&a)).await.unwrap();
    assert_eq!(wait.await.unwrap().unwrap().status, StepStatus::Succeeded);
    s.close().await;
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn terminal_callbacks_are_fenced_and_require_cleanup_and_frozen_outputs() {
    let (_h, w, r) = setup().await;
    let g = Arc::new(Guardian::default());
    let s = service(&w, &r, g.clone());
    s.fn_call(request(None, true)).await.unwrap();
    let a = g.get().await;
    let mut no_cleanup = completion(&a);
    no_cleanup.processes_gone = false;
    assert!(complete(&w, no_cleanup).await.is_err());
    let mut wrong = completion(&a);
    wrong.attempt = AttemptId::new();
    assert!(!complete(&w, wrong).await.unwrap());
    let mut bad = completion(&a);
    bad.outputs = inputs(json!({"value":3}));
    assert!(complete(&w, bad.clone()).await.unwrap());
    let status = call_status(&r, a.call, None).await.unwrap();
    assert_eq!(status.status, StepStatus::Failed);
    assert!(matches!(status.error, Some(PublicError::Invalid { .. })));
    assert!(complete(&w, bad).await.unwrap());
    assert!(
        !complete(
            &w,
            CallCompletion {
                completion_id: "other".into(),
                ..completion(&a)
            }
        )
        .await
        .unwrap()
    );
    assert_eq!(
        call_status(&r, a.call, None).await.unwrap().status,
        StepStatus::Failed
    );
    s.close().await;
    w.shutdown().await.unwrap();
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

struct HostGuardian {
    writer: Writer,
    home: PathBuf,
    admitted: Mutex<Option<AdmittedCall>>,
    unit: Mutex<Option<String>>,
    ready: tokio::sync::Notify,
}
impl Drop for HostGuardian {
    fn drop(&mut self) {
        if let Some(unit) = self.unit.lock().unwrap().as_ref() {
            let _ = std::process::Command::new("/usr/bin/systemctl")
                .args(["--user", "stop", unit])
                .output();
        }
    }
}
impl CallGuardian for HostGuardian {
    async fn launch(&self, call: AdmittedCall) -> Result<(), PublicError> {
        use sluice_process::{
            identity::ProcessIdentity,
            systemd::{ServiceCommand, StartOutcome, TransientService},
        };
        let mut unit = TransientService::for_test(call.call);
        let mut spec = ServiceCommand::new(std::env::current_exe().unwrap());
        spec.args = [
            "--ignored",
            "--exact",
            "direct_guardian_worker",
            "--nocapture",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let dir = self.home.join(call.call.to_string());
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("request.json"), serde_json::to_vec(&call).unwrap()).unwrap();
        spec.env
            .insert("SLUICE_P3_CALL_DIR".into(), dir.clone().into_os_string());
        // Only this freshly allocated test unit is ever eligible for cleanup.
        *self.unit.lock().unwrap() = Some(unit.name().into());
        let StartOutcome::Confirmed { state, .. } =
            unit.start_once(&spec)
                .await
                .map_err(|e| PublicError::ProcessLost {
                    message: e.to_string(),
                })?
        else {
            return Err(PublicError::ProcessLost {
                message: "test guardian start uncertain".into(),
            });
        };
        let identity = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(bytes) = std::fs::read(dir.join("identity.json")) {
                    break serde_json::from_slice::<ProcessIdentity>(&bytes).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(state.main_pid, Some(identity.pid));
        assert!(identity.cgroup.starts_with(state.cgroup.as_ref().unwrap()));
        assert!(
            claim(
                &self.writer,
                call.clone(),
                sluice_store::attempts::GuardianIdentity {
                    unit_name: unit.name().into(),
                    boot_id: identity.boot_id,
                    pid: identity.pid,
                    start: identity.start_time.to_string(),
                    cgroup: identity.cgroup,
                    socket_challenge: call.attempt.to_string()
                }
            )
            .await?
        );
        assert!(started(&self.writer, call.call, call.attempt).await?);
        std::fs::write(dir.join("started"), "").unwrap();
        *self.admitted.lock().unwrap() = Some(call);
        self.ready.notify_one();
        Ok(())
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real systemd user service and killed direct client; scratch home only"]
async fn killed_direct_client_leaves_guardian_running_and_completes_durably() {
    use tokio::io::AsyncBufReadExt;
    let (h, w, r) = setup().await;
    let guardian = Arc::new(HostGuardian {
        writer: w.clone(),
        home: h.0.clone(),
        admitted: Mutex::new(None),
        unit: Mutex::new(None),
        ready: tokio::sync::Notify::new(),
    });
    let calls = Arc::new(Calls::new(
        w.clone(),
        r.clone(),
        Arc::new(Registry::default()),
        guardian.clone(),
    ));
    let socket = h.0.join("caller.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    struct Client(std::process::Child);
    impl Drop for Client {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut client = Client(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "direct_caller_worker",
                "--nocapture",
            ])
            .env("SLUICE_P3_CALL_SOCKET", &socket)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let (connection, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut connection = tokio::io::BufReader::new(connection);
    let mut body = String::new();
    connection.read_line(&mut body).await.unwrap();
    let req: FnCall = serde_json::from_str(&body).unwrap();
    let service = calls.clone();
    let waiter = tokio::spawn(async move { service.fn_call(req).await });
    tokio::time::timeout(Duration::from_secs(5), guardian.ready.notified())
        .await
        .unwrap();
    let admitted = guardian.admitted.lock().unwrap().clone().unwrap();
    client.0.kill().unwrap();
    assert!(!client.0.wait().unwrap().success());
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    drop(connection);
    let unit = guardian.unit.lock().unwrap().clone().unwrap();
    let active = std::process::Command::new("/usr/bin/systemctl")
        .args(["--user", "is-active", &unit])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(active.stdout).unwrap().trim(), "active");
    assert_eq!(
        call_status(&r, admitted.call, None).await.unwrap().status,
        StepStatus::Running
    );
    let dir = h.0.join(admitted.call.to_string());
    std::fs::write(dir.join("go"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = std::process::Command::new("/usr/bin/systemctl")
                .args(["--user", "show", "--property=ActiveState", "--value", &unit])
                .output()
                .unwrap();
            let state = String::from_utf8(state.stdout).unwrap();
            if matches!(state.trim(), "inactive" | "failed") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let outputs: JsonMap =
        serde_json::from_slice(&std::fs::read(dir.join("completion.json")).unwrap()).unwrap();
    let mut done = completion(&admitted);
    done.outputs = outputs;
    assert!(complete(&w, done).await.unwrap());
    assert_eq!(
        call_status(&r, admitted.call, None).await.unwrap().status,
        StepStatus::Succeeded
    );
    calls.close().await;
    drop(calls);
    drop(guardian);
    w.shutdown().await.unwrap();
}
#[test]
#[ignore = "internal scratch guardian worker"]
fn direct_guardian_worker() {
    let Some(dir) = std::env::var_os("SLUICE_P3_CALL_DIR").map(PathBuf::from) else {
        return;
    };
    let dir = sluice_process::host::guard_scratch_home(&dir).unwrap();
    let call: AdmittedCall =
        serde_json::from_slice(&std::fs::read(dir.join("request.json")).unwrap()).unwrap();
    let identity = sluice_process::identity::ProcessIdentity::read(std::process::id()).unwrap();
    std::fs::write(
        dir.join("identity.json"),
        serde_json::to_vec(&identity).unwrap(),
    )
    .unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while !dir.join("started").exists() || !dir.join("go").exists() {
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    use std::io::Write;
    let temp = dir.join("completion.tmp");
    let mut file = std::fs::File::create(&temp).unwrap();
    file.write_all(&serde_json::to_vec(&call.inputs).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    std::fs::rename(temp, dir.join("completion.json")).unwrap();
    std::fs::File::open(&dir).unwrap().sync_all().unwrap();
}
#[test]
#[ignore = "internal disposable direct caller"]
fn direct_caller_worker() {
    let Some(path) = std::env::var_os("SLUICE_P3_CALL_SOCKET") else {
        return;
    };
    use std::io::{Read, Write};
    let mut socket = std::os::unix::net::UnixStream::connect(path).unwrap();
    let mut req = request(None, true);
    req.wait_seconds = Some(3600);
    writeln!(socket, "{}", serde_json::to_string(&req).unwrap()).unwrap();
    let mut byte = [0];
    let _ = socket.read(&mut byte);
}
