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
    b.complete(CompletionJournal {
        protocol: 1,
        identity: l.identity.clone(),
        completion_id: "delivery-terminal".into(),
        result: PayloadResult::Succeeded(decode_json(br#"{"value":1,"submitted":true}"#).unwrap()),
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
    assert!(
        b.guardian(
            C::Messages {
                identity: l.identity.clone(),
                after: l.assigned.through,
                through: None,
                limit: 128,
            },
            Some(&l.capability)
        )
        .await
        .is_err()
    );
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

#[tokio::test]
async fn cancel_after_payload_grant_before_started_must_settle() {
    let (home, b, f, p) = setup().await;
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
        .await
        .unwrap();
    // OS grant succeeded, but its asynchronous Started report has not committed.
    b.command(request(json!({"command":"step_cancel","args":{"project":{"kind":"id","value":p},"selection":{"steps":["work"],"tags":null},"reason":"cancel at grant boundary","author":"test"}}))).await.unwrap();
    let journal = CompletionJournal {
        protocol: 1,
        identity: l.identity.clone(),
        completion_id: "cancel-race".into(),
        result: PayloadResult::Cancelled("cancel intent".into()),
        starts: vec![sluice_process::journal::StartEvidence {
            invocation: l.invocation.invocation,
            executor: executor(),
        }],
        exits: vec![],
        cleanup: vec![CleanupEvidence {
            cgroup: "fake".into(),
            empty: true,
            escalated: false,
        }],
        submissions: JsonMap::default(),
        submission_version: None,
        delivery_acks: vec![],
    };
    let dir = home.path().join("runs").join(l.identity.run.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    journal.write(&dir).unwrap();
    let result = b.complete(journal.clone()).await;
    let replay = b.complete(journal).await;
    let phase: String = b
        .reads()
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT phase FROM attempts WHERE attempt_id=?1",
                [l.identity.attempt.to_string()],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    eprintln!("completion={result:?}; replay={replay:?}; phase={phase}");
    assert!(
        result.is_ok(),
        "A cleaned-up cancellation must terminalize even if Started lost the cancellation race"
    );
    assert!(
        replay.is_ok(),
        "Completion replay must acknowledge the same cancellation"
    );
    assert_eq!(phase, "terminal");
}

#[tokio::test]
async fn cancelled_start_records_history_without_dispatch_and_adoption_settles() {
    let (home, b, f, p) = setup().await;
    b.command(request(json!({"command":"message_post","args":{"project":{"kind":"id","value":p},"body":"feedback","to":"work","from":"test","needs_reply":false}}))).await.unwrap();
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
        .await
        .unwrap();
    b.command(request(json!({"command":"step_cancel","args":{"project":{"kind":"id","value":p},"selection":{"steps":["work"],"tags":null},"reason":"grant race","author":"test"}}))).await.unwrap();
    for _ in 0..2 {
        assert!(matches!(
            b.guardian(
                C::Started {
                    identity: l.identity.clone(),
                    invocation: l.invocation.invocation,
                    executor: executor()
                },
                Some(&l.capability)
            )
            .await,
            Err(PublicError::Cancelled { .. })
        ));
    }
    let journal = CompletionJournal {
        protocol: 1,
        identity: l.identity.clone(),
        completion_id: "import-cancel".into(),
        result: PayloadResult::Cancelled("intent".into()),
        starts: vec![sluice_process::journal::StartEvidence {
            invocation: l.invocation.invocation,
            executor: executor(),
        }],
        exits: vec![],
        cleanup: vec![CleanupEvidence {
            cgroup: "fake".into(),
            empty: true,
            escalated: false,
        }],
        submissions: JsonMap::default(),
        submission_version: None,
        delivery_acks: vec![],
    };
    let dir = home.path().join("runs").join(l.identity.run.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    journal.write(&dir).unwrap();
    b.adopt().await.unwrap();
    let run = l.identity.run;
    let (phase, cursor, starts): (String, i64, i64) = b.reads().snapshot(move |sql| {
        Ok(sql.query_row("SELECT a.phase,s.delivery_cursor,json_array_length(json_extract(a.request,'$.runtime_starts')) FROM runs r JOIN attempts a USING(attempt_id) JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id WHERE r.run_id=?1", [run.to_string()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
    }).await.unwrap();
    assert_eq!(phase, "terminal");
    assert_eq!(cursor, l.assigned.through.0);
    assert_eq!(starts, 1);
    assert!(!dir.join("completion.json").exists());
    assert_eq!(f.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn terminal_callbacks_replay_exact_replies_and_refuse_new_mutations() {
    let (_home, b, f, p) = setup().await;
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
        .await
        .unwrap();
    let post = RpcRequest {
        protocol: 1,
        request_id: RequestId("note".into()),
        run_capability: Some(l.capability.clone()),
        command: request(
            json!({"command":"message_post","args":{"project":{"kind":"id","value":p},"body":"once","run":l.identity.run,"from":"work","needs_reply":false}}),
        ),
    };
    let first = b
        .guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(post.clone()),
            },
            Some(&l.capability),
        )
        .await
        .unwrap();
    b.complete(CompletionJournal {
        protocol: 1,
        identity: l.identity.clone(),
        completion_id: "callback-terminal".into(),
        result: PayloadResult::Succeeded(decode_json(br#"{"value":1,"submitted":true}"#).unwrap()),
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
    let replay = b
        .guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(post.clone()),
            },
            Some(&l.capability),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(replay).unwrap()
    );
    let mut fresh = post;
    fresh.request_id = RequestId("new-note".into());
    assert!(
        b.guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(fresh)
            },
            Some(&l.capability)
        )
        .await
        .is_err()
    );
    let count: i64 = b
        .reads()
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT count(*) FROM messages WHERE project_id=?1 AND body='once'",
                [p.to_string()],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn project_call_sections_poll_in_drain_and_release_only_their_owner() {
    let (_home, b, f, p) = setup().await;
    b.command(request(json!({"command":"project_update","args":{"project":{"kind":"id","value":p},"resources":{"section":1},"author":"test"}}))).await.unwrap();
    for value in [1, 2] {
        b.command(request(json!({"command":"fn_call","args":{"name":"fixture.echo","inputs":{"value":value},"project":{"kind":"id","value":p},"wait_seconds":0,"direct":true,"author":"test"}}))).await.unwrap();
    }
    let launches = f.0.lock().unwrap().clone();
    let mut callbacks = vec![];
    for (index, l) in launches.iter().enumerate() {
        assert!(l.identity.step.is_none());
        b.guardian(C::Claim(guardian(l)), Some(&l.capability))
            .await
            .unwrap();
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
        callbacks.push(RpcRequest {
            protocol:1, request_id:RequestId(format!("section-{index}")), run_capability:Some(l.capability.clone()),
            command:request(json!({"command":"acquire_lease","args":{"run":l.identity.run,"resource":"section","amount":1,"priority":999,"request_id":"section"}})),
        });
    }
    let acquire = |i: usize| C::Callback {
        identity: launches[i].identity.clone(),
        request: Box::new(callbacks[i].clone()),
    };
    let R::Callback(first) = b
        .guardian(acquire(0), Some(&launches[0].capability))
        .await
        .unwrap()
    else {
        panic!()
    };
    let CommandReply::Lease {
        lease: first,
        state: sluice_model::commands::LeaseState::Held,
    } = *first
    else {
        panic!()
    };
    let R::Callback(second) = b
        .guardian(acquire(1), Some(&launches[1].capability))
        .await
        .unwrap()
    else {
        panic!()
    };
    let CommandReply::Lease {
        lease: second,
        state: sluice_model::commands::LeaseState::Waiting,
    } = *second
    else {
        panic!()
    };
    assert_ne!(first, second);
    b.writer()
        .write(sluice_store::RetrySafety::Idempotent, move |tx| {
            tx.sql()
                .execute("UPDATE maintenance SET mode='drain' WHERE singleton=1", [])?;
            tx.changed(None, "maintenance");
            Ok(())
        })
        .await
        .unwrap();
    for (index, l) in launches.iter().enumerate() {
        b.complete(CompletionJournal {
            protocol: 1,
            identity: l.identity.clone(),
            completion_id: format!("section-stop-{index}"),
            result: PayloadResult::Succeeded(
                decode_json(&serde_json::to_vec(&json!({"value":index+1})).unwrap()).unwrap(),
            ),
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
        if index == 0 {
            let R::Callback(reply) = b
                .guardian(acquire(1), Some(&launches[1].capability))
                .await
                .unwrap()
            else {
                panic!()
            };
            assert!(
                matches!(*reply, CommandReply::Lease { lease, state:sluice_model::commands::LeaseState::Held } if lease==second)
            );
            // A terminal acquisition replay returns its last exact cached reply.
            assert!(matches!(
                b.guardian(acquire(0), Some(&launches[0].capability))
                    .await
                    .unwrap(),
                R::Callback(_)
            ));
        }
    }
    let held = b
        .reads()
        .snapshot(move |sql| sluice_store::resources::held(sql, p))
        .await
        .unwrap();
    assert!(held.is_empty());
}
