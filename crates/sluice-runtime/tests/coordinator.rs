#[path = "../../../tests/support/home.rs"]
mod home;
use serde_json::{Value, json};
use sluice_model::{
    RuntimeApi,
    commands::*,
    error::PublicError,
    ids::*,
    rpc::{FnInvocation, JsonMap, RequestId, RpcRequest, decode_json},
};
use sluice_process::{
    guardian::FnHost,
    identity::ProcessIdentity,
    journal::{CleanupEvidence, CompletionJournal, PayloadResult},
    socket::{CoordinatorCommand as C, CoordinatorReply as R, GuardianIdentity},
};
use sluice_runtime::{
    client::CoordinatorClient,
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
    scheduler::reconcile_project,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Vec<Launch>>>);
impl FnHost for Fake {
    async fn invoke(&self, _: FnInvocation) -> Result<JsonMap, PublicError> {
        Ok(JsonMap::default())
    }
}
impl sluice_process::guardian::AdoptionHost for Fake {
    async fn reconcile(
        &self,
        _: &sluice_process::guardian::AdoptionAttempt,
    ) -> std::io::Result<sluice_process::guardian::GuardianPresence> {
        Ok(sluice_process::guardian::GuardianPresence::Ambiguous(
            "fake host owns active work".into(),
        ))
    }
}
impl ExecutionHost for Fake {
    async fn launch(&self, l: Launch) -> Result<LaunchOutcome, PublicError> {
        self.0.lock().unwrap().push(l);
        Ok(LaunchOutcome::Accepted)
    }
    async fn cleanup_valid(&self, j: &CompletionJournal) -> Result<bool, PublicError> {
        Ok(j.cleanup.iter().all(|p| p.empty))
    }
}
fn request(v: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&v).unwrap()).unwrap()
}
async fn setup() -> (home::ScratchHome, Coordinator<Fake>, Fake, ProjectId) {
    let home = home::ScratchHome::new().unwrap();
    assert!(home.root().exists());
    home::ScratchHome::validate(home.path()).unwrap();
    let fake = Fake::default();
    let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), fake.clone())
        .await
        .unwrap();
    let CommandReply::Project(p)=broker.command(request(json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":{},"author":"test"}}))).await.unwrap()else{panic!("project")};
    broker.command(request(json!({"command":"plan_patch","args":{"project":json!({"kind":"id","value":p.project_id}),"rev":1,"ops":[{"op":"add","path":"/steps/work","value":{"run":"fixture.submit","in":{"value":{"default":1}},"outputs":{"submitted":"boolean"}}}],"start":true,"dry_run":false,"reason":"test","author":"test"}}))).await.unwrap();
    (home, broker, fake, p.project_id)
}
fn guardian(l: &Launch) -> GuardianIdentity {
    GuardianIdentity {
        identity: l.identity.clone(),
        process: ProcessIdentity {
            pid: 123,
            start_time: 42,
            boot_id: "fixture-boot".into(),
            cgroup: format!("/fake/sluice-test-{}.service/control", l.identity.run),
        },
        unit: format!("sluice-test-{}.service", l.identity.run),
        socket_challenge: "challenge".into(),
    }
}
fn executor() -> ProcessIdentity {
    ProcessIdentity {
        pid: 124,
        start_time: 43,
        boot_id: "fixture-boot".into(),
        cgroup: "/fake/payload".into(),
    }
}
#[tokio::test]
async fn claim_replay_is_same_identity_only_and_start_consumes_backlog_once() {
    let (_home, b, f, p) = setup().await;
    b.command(request(json!({"command":"message_post","args":{"project":json!({"kind":"id","value":p}),"body":"feedback","thread":"step-work","to":"work","needs_reply":false,"reply_to":null,"answer":null,"title":null,"ui":null,"input":null,"data":null,"from":"test","run":null,"author":"test"}}))).await.unwrap();
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    assert!(l.assigned.through.0 > 0);
    for _ in 0..2 {
        assert!(matches!(
            b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
                .await
                .unwrap(),
            R::Claimed(true)
        ));
    }
    let mut other = guardian(&l);
    other.process.start_time += 1;
    assert!(matches!(
        b.guardian(C::Claim(other), Some(&l.capability))
            .await
            .unwrap(),
        R::Claimed(false)
    ));
    assert!(
        b.guardian(
            C::DeliverAck {
                identity: l.identity.clone(),
                ack: sluice_process::journal::DeliveryAck {
                    invocation: InvocationId::new(),
                    message: l.assigned.through
                },
            },
            Some(&l.capability)
        )
        .await
        .is_err()
    );
    let cursor = b
        .reads()
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT delivery_cursor FROM steps WHERE project_id=?1",
                [p.to_string()],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(cursor, 0);
    for _ in 0..2 {
        b.guardian(
            C::Started {
                identity: l.identity.clone(),
                invocation: l.invocation.invocation,
                executor: executor(),
            },
            Some(&l.capability),
        )
        .await
        .unwrap();
    }
    let cursor = b
        .reads()
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT delivery_cursor FROM steps WHERE project_id=?1",
                [p.to_string()],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(cursor, l.assigned.through.0);
    b.guardian(
        C::DeliverAck {
            identity: l.identity.clone(),
            ack: sluice_process::journal::DeliveryAck {
                invocation: l.invocation.invocation,
                message: l.assigned.through,
            },
        },
        Some(&l.capability),
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn callbacks_deduplicate_atomically_and_completion_replay_does_not_release_replacement() {
    let (_home, b, f, p) = setup().await;
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
        .await
        .unwrap();
    let callback = RpcRequest {
        protocol: 1,
        request_id: RequestId("submit-once".into()),
        run_capability: Some(l.capability.clone()),
        command: request(
            json!({"command":"step_submit","args":{"project":p,"step":"work","run":l.identity.run,"outputs":{"submitted":true},"author":"fixture"}}),
        ),
    };
    for _ in 0..2 {
        b.guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(callback.clone()),
            },
            Some(&l.capability),
        )
        .await
        .unwrap();
    }
    let submitted = b.submissions(l.identity.run).await.unwrap();
    assert_eq!(submitted.version, Some(1));
    let mut changed = callback.clone();
    changed.command = request(
        json!({"command":"step_submit","args":{"project":p,"step":"work","run":l.identity.run,"outputs":{"submitted":false},"author":"fixture"}}),
    );
    assert!(
        b.guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(changed)
            },
            Some(&l.capability)
        )
        .await
        .is_err()
    );
    let journal = CompletionJournal {
        protocol: 1,
        identity: l.identity.clone(),
        completion_id: "one".into(),
        result: PayloadResult::Succeeded(decode_json(br#"{"value":1}"#).unwrap()),
        starts: vec![],
        exits: vec![],
        cleanup: vec![CleanupEvidence {
            cgroup: "fake".into(),
            empty: true,
            escalated: false,
        }],
        submissions: submitted.fields,
        submission_version: submitted.version,
        delivery_acks: vec![],
    };
    b.complete(journal.clone()).await.unwrap();
    b.complete(journal.clone()).await.unwrap();
    assert!(matches!(
        b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
            .await
            .unwrap(),
        R::Claimed(false)
    ));
    b.command(request(json!({"command":"step_retry","args":{"project":json!({"kind":"id","value":p}),"selection":{"steps":["work"],"tags":null},"message":null,"reason":"retry","author":"test"}}))).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    b.complete(journal).await.unwrap();
    let status=b.command(request(json!({"command":"status","args":{"project":json!({"kind":"id","value":p}),"selection":{"steps":null,"tags":null}}}))).await.unwrap();
    let CommandReply::Data(status) = status else {
        panic!("status")
    };
    assert_eq!(status.as_value()["steps"]["work"]["status"], "running");
}
#[tokio::test]
async fn cancellation_prevents_claim_and_bad_capability_cannot_read_run() {
    let (_home, b, f, p) = setup().await;
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    assert!(
        b.guardian(
            C::Submissions(l.identity.clone()),
            Some(&sluice_model::rpc::RunCapability::new("wrong"))
        )
        .await
        .is_err()
    );
    b.command(request(json!({"command":"step_cancel","args":{"project":json!({"kind":"id","value":p}),"selection":{"steps":["work"],"tags":null},"reason":"cancel","author":"test"}}))).await.unwrap();
    assert!(matches!(
        b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
            .await
            .unwrap(),
        R::Claimed(false)
    ));
}
#[tokio::test]
async fn socket_lease_has_one_holder_and_disconnect_releases_it() {
    use std::os::unix::fs::PermissionsExt;
    let (home, b, _, _) = setup().await;
    let stop = tokio_util::sync::CancellationToken::new();
    let cloned = b.clone();
    let token = stop.clone();
    let task = tokio::spawn(async move { cloned.serve(token).await });
    let client = CoordinatorClient::new(home.path());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !client.path.exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        std::fs::metadata(&client.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let first = client.acquire_scheduler().await.unwrap();
    assert!(client.acquire_scheduler().await.is_err());
    drop(first);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while b.scheduler_owner().await.unwrap().is_some() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let second = client.acquire_scheduler().await.unwrap();
    assert!(matches!(
        client.command(CommandRequest::ProjectsList).await.unwrap(),
        CommandReply::Projects(_)
    ));
    drop(second);
    stop.cancel();
    task.await.unwrap().unwrap();
}
#[tokio::test]
async fn direct_call_handoff_survives_cancelled_waiter_and_queued_call_stays_pending() {
    let (_home, b, f, p) = setup().await;
    let queued=b.command(request(json!({"command":"fn_call","args":{"name":"fixture.echo","inputs":{"value":1},"project":json!({"kind":"id","value":p}),"wait_seconds":0,"direct":false,"author":"test"}}))).await.unwrap();
    let CommandReply::Data(queued) = queued else {
        panic!("call")
    };
    assert_eq!(queued.as_value()["status"], "pending");
    assert!(f.0.lock().unwrap().is_empty());
    let broker = b.clone();
    let waiter = tokio::spawn(async move {
        broker.command(request(json!({"command":"fn_call","args":{"name":"fixture.echo","inputs":{"value":2},"project":json!({"kind":"id","value":p}),"wait_seconds":60,"direct":true,"author":"test"}}))).await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while f.0.lock().unwrap().is_empty() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    waiter.abort();
    let _ = waiter.await;
    let l = f.0.lock().unwrap()[0].clone();
    assert!(l.identity.step.is_none());
    b.complete(CompletionJournal {
        protocol: 1,
        identity: l.identity.clone(),
        completion_id: "direct".into(),
        result: PayloadResult::Succeeded(decode_json(br#"{"value":2}"#).unwrap()),
        starts: vec![],
        exits: vec![],
        cleanup: vec![CleanupEvidence {
            cgroup: "fake".into(),
            empty: true,
            escalated: false,
        }],
        submissions: JsonMap::default(),
        submission_version: None,
        delivery_acks: vec![],
    })
    .await
    .unwrap();
    assert_eq!(
        sluice_runtime::calls::call_status(b.reads(), l.identity.run, Some(p))
            .await
            .unwrap()
            .status,
        StepStatus::Succeeded
    );
}
