use super::home;
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
struct Fake(
    Arc<Mutex<Vec<Launch>>>,
    Arc<tokio::sync::Notify>,
    Arc<std::sync::atomic::AtomicBool>,
    /// Each live run's cgroup counters, as the OS host reads them from the event files.
    Arc<Mutex<std::collections::BTreeMap<RunId, sluice_model::RunUsage>>>,
    /// The host's memory as the OS host reads it from /proc; `None` measures nothing.
    Arc<Mutex<Option<sluice_runtime::headroom::HostReading>>>,
    /// Each live run's working memory, charged memory and kernel peak.
    Arc<Mutex<std::collections::BTreeMap<RunId, sluice_model::limits::RunMemory>>>,
    /// Seconds the host's clock is ahead of the wall clock.
    Arc<std::sync::atomic::AtomicU64>,
);
impl Fake {
    async fn launched(&self, count: usize) -> Vec<Launch> {
        loop {
            let launched = self.1.notified();
            tokio::pin!(launched);
            launched.as_mut().enable();
            {
                let launches = self.0.lock().unwrap();
                if launches.len() >= count {
                    return launches.clone();
                }
            }
            launched.await;
        }
    }
}
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
    async fn terminal_gone(&self, _: RunId, stop: bool) -> Result<bool, PublicError> {
        if stop {
            self.2.store(false, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(!self.2.load(std::sync::atomic::Ordering::SeqCst))
    }

    async fn launch(&self, l: Launch) -> Result<LaunchOutcome, PublicError> {
        self.0.lock().unwrap().push(l);
        self.1.notify_waiters();
        Ok(LaunchOutcome::Accepted)
    }
    async fn cleanup_valid(&self, j: &CompletionJournal) -> Result<bool, PublicError> {
        Ok(j.cleanup.iter().all(|p| p.empty))
    }
    async fn usage(&self, run: RunId, _: &str) -> Option<sluice_model::RunUsage> {
        self.3.lock().unwrap().get(&run).copied()
    }
    async fn host_reading(
        &self,
        _: HomeId,
    ) -> Option<std::io::Result<sluice_runtime::headroom::HostReading>> {
        self.4.lock().unwrap().map(Ok)
    }
    async fn run_memory(
        &self,
        run: RunId,
        _: &str,
    ) -> Option<std::io::Result<sluice_model::limits::RunMemory>> {
        self.5.lock().unwrap().get(&run).copied().map(Ok)
    }
    fn now(&self) -> std::time::SystemTime {
        std::time::SystemTime::now()
            + Duration::from_secs(self.6.load(std::sync::atomic::Ordering::SeqCst))
    }
}
fn request(v: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&v).unwrap()).unwrap()
}
async fn setup() -> (home::ScratchHome, Coordinator<Fake>, Fake, ProjectId) {
    let home = finite_home();
    assert!(home.root().exists());
    home::ScratchHome::validate(home.path()).unwrap();
    let fake = Fake::default();
    let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), fake.clone())
        .await
        .unwrap();
    let CommandReply::Project(p)=broker.command(request(json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":{},"author":"test"}}))).await.unwrap()else{panic!("project")};
    broker.command(request(json!({"command":"plan_edit","args":{"project":json!({"kind":"id","value":p.project_id}),"rev":1,"ops":[{"op":"step.add","step":"work","spec":{"run":"fixture.submit","in":{"value":{"default":1}},"outputs":{"submitted":"boolean"}}}],"start":true,"dry_run":false,"reason":"test","author":"test"}}))).await.unwrap();
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
async fn claim_replay_is_same_identity_only_and_start_consumes_backlog_once(
    b: Coordinator<Fake>,
    f: Fake,
    p: ProjectId,
) {
    b.command(request(json!({"command":"say","args":{"project":json!({"kind":"id","value":p}),"body":"feedback","to":"work","data":null,"run":null,"me":"@orch"}}))).await.unwrap();
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
        limits: l.limits,
        guardian_memory_bytes: l.policy.guardian_memory_bytes,
        usage: Some(Default::default()),
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
/// One ask, say or reply through the coordinator's command dispatch: its receipt. A caller
/// that is neither the owner nor a run speaks as the session `@orch` unless it names `me`.
async fn speak(
    b: &Coordinator<Fake>,
    p: ProjectId,
    verb: &str,
    args: Value,
) -> Result<MessageReceipt, PublicError> {
    let mut args = args;
    args["project"] = json!({"kind":"id","value":p});
    if args.get("owner").is_none() && args.get("run").is_none() && args.get("me").is_none() {
        args["me"] = json!("@orch");
    }
    match b
        .command(request(json!({"command":verb,"args":args})))
        .await?
    {
        CommandReply::Receipt(receipt) => Ok(receipt),
        reply => panic!("{verb}: {reply:?}"),
    }
}
async fn read_messages(b: &Coordinator<Fake>, p: ProjectId, args: Value) -> Vec<Message> {
    let mut args = args;
    args["project"] = json!({"kind":"id","value":p});
    match b
        .command(request(json!({"command":"messages","args":args})))
        .await
        .unwrap()
    {
        CommandReply::Messages(page) => page.messages,
        reply => panic!("messages: {reply:?}"),
    }
}
async fn message_count(b: &Coordinator<Fake>, p: ProjectId) -> (i64, i64) {
    b.reads()
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT (SELECT count(*) FROM messages WHERE project_id=?1),(SELECT count(*) FROM records WHERE project_id=?1 AND kind='message')",
                [p.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap()
}
async fn ask_say_and_reply_reach_every_recipient_on_derived_threads(
    b: Coordinator<Fake>,
    _f: Fake,
    p: ProjectId,
) {
    // A session (`@orch`) and the owner (the dashboard) speak to every recipient kind: a step
    // of the plan, a session and the owner.
    let say_step = speak(&b, p, "say", json!({"to":"work","body":"steer"}))
        .await
        .unwrap();
    assert_eq!(
        (
            say_step.to.as_str(),
            say_step.thread.as_str(),
            say_step.delivery,
            say_step.run
        ),
        ("work", "step-work", Delivery::Queued, None)
    );
    let ask_owner = speak(
        &b,
        p,
        "ask",
        json!({"to":"owner","body":"ship it?","title":"Ship"}),
    )
    .await
    .unwrap();
    // A session and the owner talk on the session's own thread.
    assert_eq!(
        (ask_owner.thread.as_str(), ask_owner.delivery),
        ("@orch", Delivery::Delivered)
    );
    let say_orch = speak(
        &b,
        p,
        "say",
        json!({"to":"@orch","body":"fyi","owner":true}),
    )
    .await
    .unwrap();
    assert_eq!(
        (say_orch.thread.as_str(), say_orch.delivery),
        ("@orch", Delivery::Delivered)
    );
    let ask_step = speak(
        &b,
        p,
        "ask",
        json!({"to":"work","body":"which db?","owner":true}),
    )
    .await
    .unwrap();
    assert_eq!(ask_step.thread, "step-work");
    let ask_orch = speak(
        &b,
        p,
        "ask",
        json!({"to":"@orch","body":"go?","owner":true}),
    )
    .await
    .unwrap();
    // A reply goes to the parent's sender on the parent's thread and answers it.
    let reply = speak(
        &b,
        p,
        "reply",
        json!({"to_message":ask_step.id,"body":"postgres"}),
    )
    .await
    .unwrap();
    assert_eq!(
        (reply.to.as_str(), reply.thread.as_str(), reply.delivery),
        ("owner", "step-work", Delivery::Delivered)
    );
    let closed = speak(
        &b,
        p,
        "reply",
        json!({"to_message":ask_owner.id,"answer":{"action":"close"},"owner":true}),
    )
    .await
    .unwrap();
    assert_eq!(
        (closed.to.as_str(), closed.thread.as_str()),
        ("@orch", "@orch")
    );
    // A ui answer to a question no longer open conflicts; a plain reply is a message.
    assert!(matches!(
        speak(&b, p, "reply", json!({"to_message":ask_owner.id,"answer":{"action":"submit","values":{"value":1}},"owner":true})).await,
        Err(PublicError::Conflict { .. })
    ));
    let after = speak(
        &b,
        p,
        "reply",
        json!({"to_message":ask_owner.id,"body":"after all","owner":true}),
    )
    .await
    .unwrap();
    // The thread shows each message's verb, derived sender and a question's state.
    let thread = read_messages(&b, p, json!({"view":"thread","thread":"step-work"})).await;
    let shape: Vec<_> = thread
        .iter()
        .map(|m| {
            (
                m.verb,
                m.from.as_str(),
                m.to.as_deref(),
                m.state,
                m.answered_by,
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            (MessageVerb::Say, "@orch", Some("work"), None, None),
            (
                MessageVerb::Ask,
                "owner",
                Some("work"),
                Some(QuestionState::Answered),
                Some(reply.id)
            ),
            (MessageVerb::Reply, "@orch", Some("owner"), None, None),
        ]
    );
    assert_eq!(thread[2].to_message, Some(ask_step.id));
    let owner = read_messages(&b, p, json!({"view":"thread","thread":"@orch"})).await;
    assert_eq!(owner[0].state, Some(QuestionState::Closed));
    assert_eq!(owner[0].answered_by, None);
    // Open questions, and each reader's own inbox.
    let open: Vec<_> = read_messages(&b, p, json!({"view":"questions"}))
        .await
        .iter()
        .map(|m| (m.id, m.state))
        .collect();
    assert_eq!(open, vec![(ask_orch.id, Some(QuestionState::Open))]);
    let inbox = |owner: bool| {
        let b = &b;
        async move {
            read_messages(b, p, json!({"view":"inbox","owner":owner}))
                .await
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
        }
    };
    // The owner's; a session's own inbox is not read by its name yet (`messages` reads the
    // inbox of what was addressed to `orchestrator`, which a session's messages never are).
    assert!(inbox(false).await.is_empty());
    assert_eq!(inbox(true).await, vec![reply.id]);
    let _ = (ask_orch, say_orch, after);
}
async fn a_missing_unknown_or_removed_recipient_is_invalid_and_stores_nothing(
    b: Coordinator<Fake>,
    _f: Fake,
    p: ProjectId,
) {
    let before = message_count(&b, p).await;
    for (verb, args) in [
        ("say", json!({"body":"x"})),
        ("say", json!({"to":"","body":"x"})),
        ("ask", json!({"to":"nobody","body":"x"})),
        ("ask", json!({"to":"Work","body":"x"})),
        ("say", json!({"to":"cli","body":"x"})),
        ("say", json!({"to":"orchestrator","body":"x"})),
        ("say", json!({"to":"@orch","body":"to myself"})),
        ("say", json!({"to":"@nobody","body":"x"})),
        ("say", json!({"to":"work","body":"x","me":"orchestrator"})),
        ("say", json!({"to":"work","body":"x","me":"owner"})),
        ("say", json!({"to":"work","body":" "})),
    ] {
        assert!(
            matches!(
                speak(&b, p, verb, args.clone()).await,
                Err(PublicError::Invalid { .. })
            ),
            "{verb} {args}"
        );
    }
    assert!(matches!(
        speak(&b, p, "reply", json!({"to_message":999999,"body":"x"})).await,
        Err(PublicError::NotFound { .. })
    ));
    assert!(matches!(
        speak(
            &b,
            p,
            "say",
            json!({"to":"work","body":"x","owner":true,"run":RunId::new()})
        )
        .await,
        Err(PublicError::Invalid { .. })
    ));
    assert_eq!(message_count(&b, p).await, before);
    b.command(request(json!({"command":"step_remove","args":{"project":{"kind":"id","value":p},"selection":{"steps":["work"],"tags":null},"edit":{"dry_run":false,"reason":"gone","author":"test"}}})))
        .await
        .unwrap();
    assert!(matches!(
        speak(&b, p, "say", json!({"to":"work","body":"x"})).await,
        Err(PublicError::Invalid { .. })
    ));
    assert_eq!(message_count(&b, p).await, before);
}
async fn receipts_follow_the_step_from_pending_through_its_run_to_done(
    b: Coordinator<Fake>,
    f: Fake,
    p: ProjectId,
) {
    b.command(request(json!({"command":"step_add","args":{"project":{"kind":"id","value":p},"step":"held","spec":{"run":"fixture.echo","paused":true,"in":{"value":{"default":1}}},"start":false,"edit":{"dry_run":false,"reason":"test","author":"test"}}})))
        .await
        .unwrap();
    let held = speak(&b, p, "say", json!({"to":"held","body":"later"}))
        .await
        .unwrap();
    assert_eq!((held.delivery, held.run), (Delivery::NoLiveRun, None));
    let pending = speak(&b, p, "say", json!({"to":"work","body":"first"}))
        .await
        .unwrap();
    assert_eq!((pending.delivery, pending.run), (Delivery::Queued, None));
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    // Reserved, not started: the run takes it on its live feed once it starts.
    let reserved = speak(&b, p, "say", json!({"to":"work","body":"second"}))
        .await
        .unwrap();
    assert_eq!(
        (reserved.delivery, reserved.run),
        (Delivery::Queued, Some(l.identity.run))
    );
    b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
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
    // A started run of a fn that does not listen: kept for a later run.
    let running = speak(&b, p, "say", json!({"to":"work","body":"third"}))
        .await
        .unwrap();
    assert_eq!((running.delivery, running.run), (Delivery::NoLiveRun, None));
    // The step's own run speaks on its thread, as its step.
    // Naming the retired orchestrator, as a run of an older release does, it asks its unit's
    // owner: unowned, with no fallback, the person.
    let own = speak(
        &b,
        p,
        "ask",
        json!({"to":"orchestrator","body":"which?","run":l.identity.run}),
    )
    .await
    .unwrap();
    assert_eq!(
        (own.to.as_str(), own.thread.as_str(), own.delivery),
        ("owner", "step-work", Delivery::Delivered)
    );
    let mine = read_messages(&b, p, json!({"view":"questions"})).await;
    assert_eq!(
        (mine[0].from.as_str(), mine[0].run),
        ("work", Some(l.identity.run))
    );
}
/// A pack fn that runs an agent and passes `listen` on, as a worker fn does: its live
/// runs listen, whatever guardian protocol they speak, unless the step binds it false.
#[tokio::test]
async fn receipts_follow_durable_listening_whether_the_guardian_polls_or_watches() {
    use sluice_model::{plan::FnSignature, types::Type};
    use sluice_process::socket::{CoordinatorLink, UnixCoordinatorLink};
    let home = finite_home();
    home::ScratchHome::validate(home.path()).unwrap();
    let f = Fake::default();
    let mut catalog = Catalog::fixtures();
    catalog.0.insert(
        "fixture.worker".into(),
        FnSignature {
            inputs: [
                ("value".into(), Type::Any),
                ("listen".into(), Type::Optional(Box::new(Type::Boolean))),
            ]
            .into(),
            outputs: [("value".into(), Type::Any)].into(),
            ..Default::default()
        },
    );
    let b = Coordinator::open(home.path().into(), catalog, f.clone())
        .await
        .unwrap();
    let CommandReply::Project(project) = b.command(request(json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":{},"author":"test"}}))).await.unwrap() else { panic!("project") };
    let p = project.project_id;
    let step = |listen: Option<bool>| {
        let mut step = json!({"run":"fixture.worker","in":{"value":{"default":1}}});
        if let Some(listen) = listen {
            step["in"]["listen"] = json!({ "default": listen });
        }
        step
    };
    b.command(request(json!({"command":"plan_edit","args":{"project":json!({"kind":"id","value":p}),"rev":1,"ops":[
        {"op":"step.add","step":"polled","spec":step(None)},
        {"op":"step.add","step":"watched","spec":step(None)},
        {"op":"step.add","step":"quiet","spec":step(Some(false))},
    ],"start":true,"dry_run":false,"reason":"test","author":"test"}}))).await.unwrap();
    let say = |to: &'static str| speak(&b, p, "say", json!({"to":to,"body":"hello"}));
    // Pending: the next run is assigned it.
    for to in ["polled", "watched", "quiet"] {
        let r = say(to).await.unwrap();
        assert_eq!((r.delivery, r.run), (Delivery::Queued, None), "{to}");
    }
    let stop = tokio_util::sync::CancellationToken::new();
    let server = tokio::spawn({
        let (b, stop) = (b.clone(), stop.clone());
        async move { b.serve(stop).await }
    });
    let socket = home.path().join("coordinator.sock");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    b.acquire_scheduler("s".into()).await.unwrap();
    // Serving owns admission; wait for its launches before speaking as guardians.
    let launches = tokio::time::timeout_at(deadline, f.launched(3))
        .await
        .unwrap();
    let launch = |name: &str| {
        launches
            .iter()
            .find(|l| l.identity.step.as_ref().map(|s| s.as_str()) == Some(name))
            .unwrap()
            .clone()
    };
    let (polled, watched, quiet) = (launch("polled"), launch("watched"), launch("quiet"));
    let link = |l: &Launch| UnixCoordinatorLink {
        path: socket.clone(),
        capability: l.capability.clone(),
    };
    for l in [&polled, &watched, &quiet] {
        let link = link(l);
        assert!(matches!(
            link.request(C::Claim(guardian(l))).await,
            Ok(R::Claimed(true))
        ));
        assert!(matches!(
            link.request(C::Started {
                identity: l.identity.clone(),
                invocation: l.invocation.invocation,
                executor: executor(),
            })
            .await,
            Ok(R::Started)
        ));
    }
    // An older guardian polls with Messages and never holds a watch.
    let r = say("polled").await.unwrap();
    assert_eq!(
        (r.delivery, r.run),
        (Delivery::Delivered, Some(polled.identity.run))
    );
    let Ok(R::Messages(offered)) = link(&polled)
        .request(C::Messages {
            identity: polled.identity.clone(),
            after: polled.assigned.through,
            through: None,
            limit: 128,
        })
        .await
    else {
        panic!("poll")
    };
    assert_eq!(offered.iter().map(|m| m.id).collect::<Vec<_>>(), vec![r.id]);
    assert!(matches!(
        link(&polled)
            .request(C::DeliverAck {
                identity: polled.identity.clone(),
                ack: sluice_process::journal::DeliveryAck {
                    invocation: polled.invocation.invocation,
                    message: r.id,
                },
            })
            .await,
        Ok(R::Ack)
    ));
    // A current guardian holds a watch over the socket; the message wakes it.
    let mut held = tokio::spawn({
        let (link, identity, after) = (
            link(&watched),
            watched.identity.clone(),
            watched.assigned.through,
        );
        async move {
            link.request(C::Watch {
                identity,
                after,
                wait_ms: 30_000,
            })
            .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut held)
            .await
            .is_err()
    );
    let r = say("watched").await.unwrap();
    assert_eq!(
        (r.delivery, r.run),
        (Delivery::Delivered, Some(watched.identity.run))
    );
    let Ok(R::Watched {
        cancelled: false,
        messages,
    }) = held.await.unwrap()
    else {
        panic!("watch")
    };
    assert_eq!(
        messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![r.id]
    );
    // A run whose step binds `listen: false` does not listen.
    let r = say("quiet").await.unwrap();
    assert_eq!((r.delivery, r.run), (Delivery::NoLiveRun, None));
    b.complete(CompletionJournal {
        protocol: 1,
        limits: polled.limits,
        guardian_memory_bytes: polled.policy.guardian_memory_bytes,
        usage: Some(Default::default()),
        identity: polled.identity.clone(),
        completion_id: "polled-done".into(),
        result: PayloadResult::Succeeded(decode_json(br#"{"value":1}"#).unwrap()),
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
    // Finished and settled: refused, not kept.
    assert!(matches!(
        say("polled").await,
        Err(PublicError::Conflict { message, .. }) if message.contains("settled")
    ));
    stop.cancel();
    server.await.unwrap().unwrap();
}
#[tokio::test]
async fn the_retired_message_post_is_bridged_for_a_run_and_refused_without_one() {
    let (_home, b, f, p) = setup().await;
    let retired = |args: Value| {
        let mut args = args;
        args["project"] = json!({"kind":"id","value":p});
        request(json!({"command":"message_post","args":args}))
    };
    let before = message_count(&b, p).await;
    for args in [
        json!({"body":"x","thread":"step-work","to":"work","needs_reply":false,"from":"test"}),
        json!({"body":"x","from":"orchestrator"}),
    ] {
        assert!(matches!(
            b.command(retired(args)).await,
            Err(PublicError::Invalid { .. })
        ));
    }
    assert_eq!(message_count(&b, p).await, before);
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    let run = l.identity.run;
    // An old agent's root post with no `to` asks its unit's owner (unowned: the person); a
    // note says it.
    let CommandReply::Posted { id: asked } = b
        .command(retired(json!({"body":"which?","thread":"step-work","from":"work","run":run,"to":null,"needs_reply":true})))
        .await
        .unwrap()
    else {
        panic!("posted")
    };
    let CommandReply::Posted { id: said } = b
        .command(retired(
            json!({"body":"fyi","from":"work","run":run,"needs_reply":false}),
        ))
        .await
        .unwrap()
    else {
        panic!("posted")
    };
    speak(
        &b,
        p,
        "reply",
        json!({"to_message":asked,"body":"that one"}),
    )
    .await
    .unwrap();
    // Through the run's callback too, as an old Python fn would.
    b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
        .await
        .unwrap();
    let callback = RpcRequest {
        protocol: 1,
        request_id: RequestId("retired".into()),
        run_capability: Some(l.capability.clone()),
        command: retired(
            json!({"body":"from the callback","from":"work","run":run,"to":"owner","needs_reply":false}),
        ),
    };
    let R::Callback(reply) = b
        .guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(callback),
            },
            Some(&l.capability),
        )
        .await
        .unwrap()
    else {
        panic!("callback")
    };
    assert!(matches!(*reply, CommandReply::Posted { .. }));
    let thread = read_messages(&b, p, json!({"view":"thread","thread":"step-work"})).await;
    let shape: Vec<_> = thread
        .iter()
        .map(|m| (m.verb, m.from.as_str(), m.to.as_deref(), m.run))
        .collect();
    assert_eq!(
        shape,
        vec![
            (MessageVerb::Ask, "work", Some("owner"), Some(run)),
            (MessageVerb::Say, "work", Some("owner"), Some(run)),
            (MessageVerb::Reply, "@orch", Some("work"), None),
            (MessageVerb::Say, "work", Some("owner"), Some(run)),
        ]
    );
    assert_eq!((thread[0].id, thread[1].id), (asked, said));
    assert_eq!(thread[0].state, Some(QuestionState::Answered));
}

async fn current_reserved_run_accepts_submissions_before_claim_and_after_feedback(
    b: Coordinator<Fake>,
    f: Fake,
    p: ProjectId,
) {
    b.acquire_scheduler("s".into()).await.unwrap();
    for round in 0..2 {
        reconcile_project(&b, p, "s").await.unwrap();
        let l = f.0.lock().unwrap()[round].clone();
        let run = l.identity.run;
        let phase: String = b
            .reads()
            .snapshot(move |sql| {
                Ok(sql.query_row(
                    "SELECT a.phase FROM runs r JOIN attempts a USING(attempt_id) WHERE r.run_id=?1",
                    [run.to_string()],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(phase, "reserved");
        let submit = request(
            json!({"command":"step_submit","args":{"project":p,"step":"work","run":run,"outputs":{"submitted":true},"author":"fixture"}}),
        );
        assert!(matches!(
            b.command(submit.clone()).await.unwrap(),
            CommandReply::Ack
        ));
        // A run submits once: its own second submission is refused, replayed or not.
        let callback = RpcRequest {
            protocol: 1,
            request_id: RequestId("reserved-submit".into()),
            run_capability: Some(l.capability.clone()),
            command: submit.clone(),
        };
        for _ in 0..2 {
            assert!(matches!(
                b.guardian(
                    C::Callback {
                        identity: l.identity.clone(),
                        request: Box::new(callback.clone()),
                    },
                    Some(&l.capability),
                )
                .await,
                Err(PublicError::Conflict { message, .. }) if message.contains("already submitted")
            ));
        }
        let submitted = b.submissions(run).await.unwrap();
        assert_eq!(submitted.version, Some(1));
        b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
            .await
            .unwrap();
        b.complete(CompletionJournal {
            protocol: 1,
            limits: l.limits,
            guardian_memory_bytes: l.policy.guardian_memory_bytes,
            usage: Some(Default::default()),
            identity: l.identity.clone(),
            completion_id: format!("reserved-submit-{round}"),
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
        })
        .await
        .unwrap();
        assert!(b.command(submit).await.is_err());
        let mut fresh = callback;
        fresh.request_id = RequestId("terminal-submit".into());
        assert!(
            b.guardian(
                C::Callback {
                    identity: l.identity.clone(),
                    request: Box::new(fresh),
                },
                Some(&l.capability),
            )
            .await
            .is_err()
        );
        assert_eq!(b.submissions(run).await.unwrap().version, Some(1));
        if round == 0 {
            b.command(request(json!({"command":"step_retry","args":{"project":{"kind":"id","value":p},"selection":{"steps":["work"],"tags":null},"message":"Continue with feedback","reason":"fixture","author":"@fixture"}}))).await.unwrap();
        }
    }
}

#[tokio::test]
async fn superseded_nonterminal_run_refuses_new_submissions() {
    for ownership in ["generation", "work_generation", "run_ids"] {
        let (_home, b, f, p) = setup().await;
        b.acquire_scheduler("s".into()).await.unwrap();
        reconcile_project(&b, p, "s").await.unwrap();
        let l = f.0.lock().unwrap()[0].clone();
        b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
            .await
            .unwrap();
        b.writer()
            .write(sluice_store::RetrySafety::Idempotent, move |tx| {
                let update = match ownership {
                    "generation" => "UPDATE steps SET generation=generation+1 WHERE project_id=?1",
                    "work_generation" => {
                        "UPDATE steps SET work_generation=work_generation+1 WHERE project_id=?1"
                    }
                    "run_ids" => "UPDATE steps SET run_ids='[]' WHERE project_id=?1",
                    _ => unreachable!(),
                };
                tx.sql().execute(update, [p.to_string()])?;
                tx.changed(Some(p), "status");
                Ok(())
            })
            .await
            .unwrap();
        let submit = request(
            json!({"command":"step_submit","args":{"project":p,"step":"work","run":l.identity.run,"outputs":{"submitted":true},"author":"fixture"}}),
        );
        assert!(b.command(submit.clone()).await.is_err(), "{ownership}");
        assert!(
            b.guardian(
                C::Callback {
                    identity: l.identity.clone(),
                    request: Box::new(RpcRequest {
                        protocol: 1,
                        request_id: RequestId("superseded-submit".into()),
                        run_capability: Some(l.capability.clone()),
                        command: submit,
                    }),
                },
                Some(&l.capability),
            )
            .await
            .is_err(),
            "{ownership}"
        );
        assert_eq!(b.submissions(l.identity.run).await.unwrap().version, None);
    }
}

async fn reported_usage(b: &Coordinator<Fake>, journal: &CompletionJournal) -> Value {
    let id = &journal.identity;
    if let Some(step) = &id.step {
        let CommandReply::Data(status) = b.command(request(json!({"command":"status","args":{"project":{"kind":"id","value":id.project.unwrap()},"selection":{"steps":null,"tags":null},"all":true}}))).await.unwrap() else { panic!("status") };
        status.as_value()["steps"][step.as_str()]["run_resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["run"] == id.run.to_string())
            .unwrap()["usage"]
            .clone()
    } else {
        serde_json::to_value(
            sluice_runtime::calls::call_status(b.reads(), id.run, id.project)
                .await
                .unwrap()
                .usage,
        )
        .unwrap()
    }
}
async fn complete_with_immutable_usage(b: &Coordinator<Fake>, journal: &CompletionJournal) {
    let mut wrong_identity = journal.clone();
    wrong_identity.identity.project = Some(ProjectId::new());
    assert!(b.complete(wrong_identity).await.is_err());
    assert_eq!(reported_usage(b, journal).await, Value::Null);
    let mut wrong_budget = journal.clone();
    wrong_budget.limits.memory_max_bytes = wrong_budget.limits.memory_max_bytes.map(|n| n + 1);
    assert!(b.complete(wrong_budget).await.is_err());
    assert_eq!(reported_usage(b, journal).await, Value::Null);
    b.complete(journal.clone()).await.unwrap();
    let original = serde_json::to_value(journal.usage).unwrap();
    assert_eq!(reported_usage(b, journal).await, original);
    let mut changed = journal.clone();
    changed.usage.as_mut().unwrap().memory_peak_bytes += 1024;
    b.complete(changed.clone()).await.unwrap();
    assert_eq!(reported_usage(b, journal).await, original);
    changed.completion_id.push_str("-stale");
    assert!(b.complete(changed).await.is_err());
    assert_eq!(reported_usage(b, journal).await, original);
}

async fn callbacks_deduplicate_atomically_and_completion_replay_does_not_release_replacement(
    b: Coordinator<Fake>,
    f: Fake,
    p: ProjectId,
) {
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
        limits: l.limits,
        guardian_memory_bytes: l.policy.guardian_memory_bytes,
        usage: Some(Default::default()),
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
    complete_with_immutable_usage(&b, &journal).await;
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
async fn cancellation_prevents_claim_and_bad_capability_cannot_read_run(
    b: Coordinator<Fake>,
    f: Fake,
    p: ProjectId,
) {
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
        tokio::task::yield_now().await;
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
        tokio::task::yield_now().await;
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
        tokio::task::yield_now().await;
    }
    waiter.abort();
    let _ = waiter.await;
    let l = f.0.lock().unwrap()[0].clone();
    assert!(l.identity.step.is_none());
    let journal = CompletionJournal {
        protocol: 1,
        limits: l.limits,
        guardian_memory_bytes: l.policy.guardian_memory_bytes,
        usage: Some(Default::default()),
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
    };
    complete_with_immutable_usage(&b, &journal).await;
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
        limits: l.limits,
        guardian_memory_bytes: l.policy.guardian_memory_bytes,
        usage: Some(Default::default()),
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
    b.command(request(json!({"command":"say","args":{"project":{"kind":"id","value":p},"body":"feedback","to":"work","data":null,"run":null,"me":"@orch"}}))).await.unwrap();
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
        limits: l.limits,
        guardian_memory_bytes: l.policy.guardian_memory_bytes,
        usage: Some(Default::default()),
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

async fn terminal_callbacks_replay_exact_replies_and_refuse_new_mutations(
    b: Coordinator<Fake>,
    f: Fake,
    p: ProjectId,
) {
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
            json!({"command":"say","args":{"project":{"kind":"id","value":p},"to":"orchestrator","body":"once","data":null,"run":l.identity.run}}),
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
        limits: l.limits,
        guardian_memory_bytes: l.policy.guardian_memory_bytes,
        usage: Some(Default::default()),
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
async fn a_run_sets_its_projects_board_and_reads_it_back() {
    let (_home, b, f, p) = setup().await;
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    b.guardian(C::Claim(guardian(&l)), Some(&l.capability))
        .await
        .unwrap();
    let set = |id: &str, project: Value, program: &str| RpcRequest {
        protocol: 1,
        request_id: RequestId(id.into()),
        run_capability: Some(l.capability.clone()),
        command: request(
            json!({"command":"board_set","args":{"project":project,"program":program,"expected_rev":0,"reason":"lane overview","author":"step:work"}}),
        ),
    };
    let reply = b
        .guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(set(
                    "board",
                    json!({"kind":"id","value":p}),
                    "root = Units()",
                )),
            },
            Some(&l.capability),
        )
        .await
        .unwrap();
    assert!(format!("{reply:?}").contains("warnings"), "{reply:?}");
    let CommandReply::Board(board) = b
        .command(request(
            json!({"command":"board_get","args":{"project":{"kind":"id","value":p}}}),
        ))
        .await
        .unwrap()
    else {
        panic!("board")
    };
    assert_eq!(board.rev, sluice_model::ids::Revision(1));
    assert_eq!(board.program.as_deref(), Some("root = Units()"));
    // Another project's board is outside the run's authority.
    let CommandReply::Project(other) = b.command(request(json!({"command":"project_create","args":{"name":"other","description":"","icon":null,"resources":{},"author":"test"}}))).await.unwrap() else {
        panic!("project")
    };
    assert!(
        b.guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(set(
                    "other",
                    json!({"kind":"id","value":other.project_id}),
                    "root = Units()"
                )),
            },
            Some(&l.capability),
        )
        .await
        .is_err()
    );
    let status = b.command(request(json!({"command":"status","args":{"project":{"kind":"id","value":p},"selection":{"steps":null,"tags":null}}}))).await.unwrap();
    let CommandReply::Data(status) = status else {
        panic!("status")
    };
    assert_eq!(status.as_value()["board_rev"], 1);
    // The board's document: the owner puts a Doc() on the board; a run then writes its own
    // project's document, with no other project's in its authority.
    let set = data_of(&b, json!({"command":"board_set","args":{"project":{"kind":"id","value":p},"program":"root = Stack([Doc(), StepStatus(\"work\")])","author":"orch"}})).await;
    assert_eq!(set, json!({"rev": 2, "warnings": []}));
    let write = |id: &str, project: Value, markdown: &str| RpcRequest {
        protocol: 1,
        request_id: RequestId(id.into()),
        run_capability: Some(l.capability.clone()),
        command: request(
            json!({"command":"board_doc_write","args":{"project":project,"markdown":markdown,"expected_rev":0,"reason":"first words","author":"step:work"}}),
        ),
    };
    let reply = b
        .guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(write(
                    "doc",
                    json!({"kind":"id","value":p}),
                    "## Phase\nMain is **green**.\n",
                )),
            },
            Some(&l.capability),
        )
        .await
        .unwrap();
    assert!(format!("{reply:?}").contains("changed"), "{reply:?}");
    assert!(
        b.guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(write(
                    "doc-other",
                    json!({"kind":"id","value":other.project_id}),
                    "x"
                )),
            },
            Some(&l.capability),
        )
        .await
        .is_err()
    );
    // The owner's path: an edit at the rev read; the board's own rev stays.
    let edited = data_of(&b, json!({"command":"board_doc_edit","args":{"project":{"kind":"id","value":p},"expected_rev":1,"edits":[{"start":2,"end":2,"text":"Main is red."}],"reason":"a red run","author":"orch"}})).await;
    assert_eq!(edited, json!({"rev": 2, "changed": true}));
    let read = data_of(
        &b,
        json!({"command":"board_doc_read","args":{"project":{"kind":"id","value":p}}}),
    )
    .await;
    assert_eq!(
        (
            &read["rev"],
            &read["markdown"],
            &read["numbered"],
            &read["author"]
        ),
        (
            &json!(2),
            &json!("## Phase\nMain is red.\n"),
            &json!("     1\t## Phase\n     2\tMain is red.\n"),
            &json!("orch")
        )
    );
    let CommandReply::Board(board) = b
        .command(request(
            json!({"command":"board_get","args":{"project":{"kind":"id","value":p}}}),
        ))
        .await
        .unwrap()
    else {
        panic!("board")
    };
    assert_eq!(board.rev, sluice_model::ids::Revision(2));
    let updates: Vec<(String, String, String)> = b
        .reads()
        .snapshot(move |sql| {
            let mut q = sql.prepare("SELECT json_extract(payload,'$.fields[0]'), json_extract(payload,'$.author'), json_extract(payload,'$.reason') FROM records WHERE project_id=?1 AND kind='project.update' ORDER BY seq")?;
            let rows = q
                .query_map([p.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert_eq!(
        updates,
        [
            (
                "board_doc".to_owned(),
                "step:work".to_owned(),
                "first words".to_owned()
            ),
            (
                "board_doc".to_owned(),
                "orch".to_owned(),
                "a red run".to_owned()
            )
        ]
    );
    // A run's plan edit that takes a step the board names gets the warning, and a repeat of
    // the same callback gets the reply its attempt kept, warning and all: no release that
    // does not know board warnings survives the schema-3 cutover (plan-rows §1).
    b.command(request(json!({"command":"step_add","args":{"project":{"kind":"id","value":p},"step":"extra","spec":{"run":"fixture.echo","in":{"value":{"default":1}}},"start":false,"edit":{"dry_run":false,"reason":"test","author":"test"}}})))
        .await
        .unwrap();
    data_of(&b, json!({"command":"board_set","args":{"project":{"kind":"id","value":p},"program":"root = Stack([Doc(), StepStatus(\"extra\")])","author":"orch"}})).await;
    let remove = RpcRequest {
        protocol: 1,
        request_id: RequestId("remove-extra".into()),
        run_capability: Some(l.capability.clone()),
        command: request(
            json!({"command":"step_remove","args":{"project":{"kind":"id","value":p},"selection":{"steps":["extra"],"tags":null},"edit":{"dry_run":false,"reason":"gone","author":"step:work"}}}),
        ),
    };
    let first = b
        .guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(remove.clone()),
            },
            Some(&l.capability),
        )
        .await
        .unwrap();
    assert!(
        format!("{first:?}").contains("StepStatus names step `extra`, which is not in the plan"),
        "{first:?}"
    );
    let replay = b
        .guardian(
            C::Callback {
                identity: l.identity.clone(),
                request: Box::new(remove),
            },
            Some(&l.capability),
        )
        .await
        .unwrap();
    assert_eq!(format!("{replay:?}"), format!("{first:?}"));
}

/// A board that names a step not in the plan is set with a warning per name; a plan edit
/// that takes away a step the board names is made, and says so in `board_warnings`.
async fn board_set_and_plan_edits_warn_for_the_steps_the_board_loses(
    b: Coordinator<Fake>,
    _f: Fake,
    p: ProjectId,
) {
    let project = json!({"kind":"id","value":p});
    b.command(request(json!({"command":"step_add","args":{"project":project,"step":"tests-main","spec":{"run":"fixture.echo","tags":["main-tests"],"in":{"value":{"default":1}}},"start":false,"edit":{"dry_run":false,"reason":"test","author":"test"}}})))
        .await
        .unwrap();
    // A renamed step, in a StepStatus and a Metric's SQL; a sender that never was a step is
    // no warning.
    let set = data_of(&b, json!({"command":"board_set","args":{"project":project,"program":"root = Stack([a, b, c, d])\na = StepStatus(\"tests-main-old\")\nb = Metric(\"Red\", \"SELECT count(*) FROM steps WHERE project_id = ? AND step_id = 'tests-main'\")\nc = LatestMessage(\"reviewer\")\nd = StepStatus(\"tag:main-tests\")","author":"orch"}})).await;
    assert_eq!(
        set,
        json!({"rev": 1, "warnings": ["line 2: StepStatus names step `tests-main-old`, which is not in the plan"]})
    );
    // Removing tests-main takes it from the Metric and leaves tag:main-tests with no step.
    let reply = b
        .command(request(json!({"command":"step_remove","args":{"project":project,"selection":{"steps":["tests-main"],"tags":null},"edit":{"dry_run":false,"reason":"renamed","author":"orch"}}})))
        .await
        .unwrap();
    let CommandReply::Edit(edit) = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(
        edit.board_warnings,
        [
            "line 3: Metric names step `tests-main`, which is not in the plan",
            "line 5: StepStatus selects `tag:main-tests`, which no plan step carries"
        ]
    );
    // An edit that takes nothing the board names says nothing.
    let reply = b
        .command(request(json!({"command":"step_add","args":{"project":project,"step":"other","spec":{"run":"fixture.echo","in":{"value":{"default":1}}},"start":false,"edit":{"dry_run":false,"reason":"test","author":"test"}}})))
        .await
        .unwrap();
    let CommandReply::Edit(edit) = reply else {
        panic!("{reply:?}")
    };
    assert!(edit.board_warnings.is_empty(), "{:?}", edit.board_warnings);
    let encoded = serde_json::to_value(CommandReply::Edit(edit)).unwrap();
    assert!(encoded["data"].get("board_warnings").is_none(), "{encoded}");
}

/// A tool's data reply.
async fn data_of(b: &Coordinator<Fake>, command: Value) -> Value {
    match b.command(request(command)).await.unwrap() {
        CommandReply::Data(data) => data.into_value(),
        other => panic!("{other:?}"),
    }
}
/// The finished journal of `l`'s run, its submissions frozen as the guardian freezes them.
async fn journal(b: &Coordinator<Fake>, l: &Launch, result: PayloadResult) -> CompletionJournal {
    let submitted = b.submissions(l.identity.run).await.unwrap();
    CompletionJournal {
        protocol: 1,
        limits: l.limits,
        guardian_memory_bytes: l.policy.guardian_memory_bytes,
        usage: Some(Default::default()),
        identity: l.identity.clone(),
        completion_id: format!("done-{}", l.identity.run),
        result,
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
    }
}
/// The fake guardian claims and starts `l`.
async fn run_started(b: &Coordinator<Fake>, l: &Launch) {
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
}

async fn a_running_step_that_has_submitted_is_finishing_in_status_step_context_and_call_status(
    b: Coordinator<Fake>,
    f: Fake,
    p: ProjectId,
) {
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    run_started(&b, &l).await;
    let selector = json!({"kind":"id","value":p});
    let status = json!({"command":"status","args":{"project":selector,"selection":{"steps":null,"tags":null}}});
    let units = json!({"command":"status","args":{"project":selector,"selection":{"steps":null,"tags":null},"view":"units"}});
    let context = json!({"command":"step_context","args":{"project":selector,"step":"work"}});
    let before = data_of(&b, status.clone()).await;
    assert_eq!(before["steps"]["work"]["status"], "running");
    assert!(
        before["steps"]["work"].get("finishing").is_none(),
        "{before}"
    );
    assert!(
        data_of(&b, context.clone())
            .await
            .get("finishing")
            .is_none()
    );
    b.command(request(json!({"command":"step_submit","args":{"project":p,"step":"work","run":l.identity.run,"outputs":{"submitted":true},"author":"fixture"}}))).await.unwrap();
    let run = l.identity.run.to_string();
    let (at, seq): (String, i64) = b
        .reads()
        .snapshot(move |sql| {
            Ok((
                sql.query_row("SELECT at FROM submissions WHERE run_id=?1", [&run], |r| {
                    r.get(0)
                })?,
                sql.query_row(
                    "SELECT seq FROM records WHERE run_id=?1 AND kind='step.submit'",
                    [&run],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap();
    let expected = json!({"since":at,"submission_seq":seq,"release":"runtime-v1"});
    let after = data_of(&b, status).await;
    assert_eq!(after["steps"]["work"]["status"], "running");
    assert_eq!(after["steps"]["work"]["finishing"], expected, "{after}");
    let rows = data_of(&b, units).await;
    let row = &rows["units"][0];
    assert_eq!(
        row["finishing"],
        json!([{"step":"work","since":at,"submission_seq":seq,"release":"runtime-v1"}]),
        "{rows}"
    );
    assert_eq!(row["steps"], "work▷");
    assert!(
        row["line"].as_str().unwrap().contains("finishing work"),
        "{row}"
    );
    assert_eq!(data_of(&b, context).await["finishing"], expected);
    // A call's run that has submitted is finishing too. Nothing stores a call's submission
    // today (step_submit names a step), so the row is written here.
    let call = RunId::new();
    let attempt = AttemptId::new();
    b.writer()
        .write(sluice_store::RetrySafety::Idempotent, move |tx| {
            tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,'executing','{}','h','now')", (attempt.to_string(), p.to_string()))?;
            tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,release_id,created_at) VALUES (?1,?2,?3,'0123456789abcdef0123456789abcdef01234567-89abcdef','now')", (call.to_string(), p.to_string(), attempt.to_string()))?;
            tx.sql().execute("INSERT INTO calls(call_id,project_id,run_id,fn,status,inputs,created_at) VALUES (?1,?2,?1,'fixture.wait','running','{}','now')", (call.to_string(), p.to_string()))?;
            tx.changed(Some(p), "calls");
            Ok(())
        })
        .await
        .unwrap();
    let call_status = json!({"command":"call_status","args":{"call":call,"project":selector}});
    let running = data_of(&b, call_status.clone()).await;
    assert_eq!(running["status"], "running");
    assert!(running.get("finishing").is_none(), "{running}");
    b.writer()
        .write(sluice_store::RetrySafety::Idempotent, move |tx| {
            tx.sql().execute("INSERT INTO submissions(run_id,project_id,outputs,at) VALUES (?1,?2,'{\"x\":1}','2026-10-05T10:00:00Z')", (call.to_string(), p.to_string()))?;
            tx.changed(Some(p), "calls");
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        data_of(&b, call_status).await["finishing"],
        json!({"since":"2026-10-05T10:00:00Z","submission_seq":null,"release":"0123456789ab"})
    );
    b.writer()
        .write(sluice_store::RetrySafety::Idempotent, move |tx| {
            tx.sql().execute(
                "UPDATE attempts SET phase='terminal' WHERE attempt_id=?1",
                [attempt.to_string()],
            )?;
            tx.sql().execute(
                "UPDATE runs SET finished_at='now',result=json_object('status','succeeded') WHERE run_id=?1",
                [call.to_string()],
            )?;
            tx.sql().execute(
                "UPDATE calls SET status='succeeded' WHERE call_id=?1",
                [call.to_string()],
            )?;
            tx.changed(Some(p), "calls");
            Ok(())
        })
        .await
        .unwrap();
}

/// A coordinator whose plan's `work` runs `agent.run` (codex) in a fresh git repository.
async fn agent_setup() -> (
    home::ScratchHome,
    Coordinator<Fake>,
    Fake,
    ProjectId,
    std::path::PathBuf,
) {
    let home = finite_home();
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let mut catalog = Catalog::fixtures();
    let agent = sluice_runtime::builtins::descriptor::find("agent.run").unwrap();
    catalog.0.insert(
        "agent.run".into(),
        sluice_model::plan::FnSignature {
            inputs: agent
                .inputs
                .iter()
                .map(|(n, t)| ((*n).into(), t.clone()))
                .collect(),
            outputs: agent
                .outputs
                .iter()
                .map(|(n, t)| ((*n).into(), t.clone()))
                .collect(),
            open: true,
            ..Default::default()
        },
    );
    let fake = Fake::default();
    let broker = Coordinator::open(home.path().into(), catalog, fake.clone())
        .await
        .unwrap();
    let CommandReply::Project(p)=broker.command(request(json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":{},"author":"test"}}))).await.unwrap()else{panic!("project")};
    broker.command(request(json!({"command":"plan_edit","args":{"project":json!({"kind":"id","value":p.project_id}),"rev":1,"ops":[{"op":"step.add","step":"work","spec":{"run":"agent.run","in":{"engine":{"default":"codex"},"cwd":{"default":repo},"spec":{"default":"Fix it"}},"outputs":{"summary":"string"}}}],"start":true,"dry_run":false,"reason":"test","author":"test"}}))).await.unwrap();
    (home, broker, fake, p.project_id, repo)
}
fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(repo)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn settle(p: ProjectId) -> CommandRequest {
    request(
        json!({"command":"step_settle","args":{"project":{"kind":"id","value":p},"step":"work","reason":"its release predates the done signal","author":"owner"}}),
    )
}

#[tokio::test]
async fn step_settle_stops_a_finishing_agent_and_succeeds_with_its_done_signal_outputs() {
    let (home, b, f, p, repo) = agent_setup().await;
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    run_started(&b, &l).await;
    // Running, but nothing submitted: it is not finishing.
    let refused = b.command(settle(p)).await;
    assert!(
        matches!(&refused, Err(PublicError::Invalid { message, .. }) if message.contains("has not submitted")),
        "{refused:?}"
    );
    // What its supervisor checkpointed, as a release from before the done signal leaves it.
    let before = git(&repo, &["rev-parse", "HEAD"]);
    let run_dir = home.path().join("runs").join(l.identity.run.to_string());
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("native.json"),
        json!({"session":"codex-session-1","final_text":"Fixed it.","head_before":before,"state":"Idle"}).to_string(),
    )
    .unwrap();
    std::fs::write(repo.join("a.txt"), "b\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "fix"]);
    let after = git(&repo, &["rev-parse", "HEAD"]);
    b.command(request(json!({"command":"step_submit","args":{"project":p,"step":"work","run":l.identity.run,"outputs":{"summary":"fixed"},"author":"fixture"}}))).await.unwrap();
    let CommandReply::Data(settled) = b.command(settle(p)).await.unwrap() else {
        panic!("settle reply")
    };
    let settled = settled.into_value();
    let outputs = settled["outputs"].clone();
    // {project, step, run, outputs}, as its description says.
    assert_eq!(
        (&settled["project"], &settled["step"], &settled["run"]),
        (&json!(p), &json!("work"), &json!(l.identity.run))
    );
    assert_eq!(outputs["summary"], "fixed");
    assert_eq!(outputs["final"], "Fixed it.");
    assert_eq!(outputs["session"], "codex-session-1");
    assert_eq!(outputs["report"], Value::Null);
    assert!(
        outputs["model"].as_str().unwrap().starts_with("gpt-"),
        "{outputs}"
    );
    assert_eq!(
        outputs["git"],
        json!({"head_before":before,"head_after":after,"commits":1,"dirty":false})
    );
    // Its guardian is told to stop it, as a cancel tells it; a second settle is refused.
    assert!(matches!(
        b.guardian(C::CancelIntent(l.identity.clone()), Some(&l.capability))
            .await
            .unwrap(),
        R::CancelIntent(true)
    ));
    let again = b.command(settle(p)).await;
    assert!(
        matches!(&again, Err(PublicError::Conflict { message, .. }) if message.contains("already being settled")),
        "{again:?}"
    );
    b.complete(journal(&b, &l, PayloadResult::Cancelled("cancel intent".into())).await)
        .await
        .unwrap();
    let status = data_of(&b, json!({"command":"status","args":{"project":{"kind":"id","value":p},"selection":{"steps":null,"tags":null},"all":true}})).await;
    let work = &status["steps"]["work"];
    assert_eq!(work["status"], "succeeded", "{work}");
    assert_eq!(work["error"], Value::Null);
    assert_eq!(work["outputs"], outputs);
    let CommandReply::Records(page) = b.command(request(json!({"command":"log_read","args":{"project":{"kind":"id","value":p},"since_seq":null,"kinds":["step.settle"],"threads":null,"limit":10}}))).await.unwrap() else {
        panic!("records")
    };
    let record = serde_json::to_value(&page.records[0]).unwrap();
    assert_eq!(
        (&record["kind"], &record["author"], &record["run"]),
        (
            &json!("step.settle"),
            &json!("owner"),
            &json!(l.identity.run)
        )
    );
}

async fn step_settle_refuses_a_fn_that_composes_its_agent_with_the_way_by_hand(
    b: Coordinator<Fake>,
    f: Fake,
    p: ProjectId,
) {
    // fixture.submit stands for a project fn that runs an agent (ctx.builtin("agent.run")):
    // its outputs are what it returns, which sluice cannot derive from the submission.

    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    run_started(&b, &l).await;
    b.command(request(json!({"command":"step_submit","args":{"project":p,"step":"work","run":l.identity.run,"outputs":{"submitted":true},"author":"fixture"}}))).await.unwrap();
    let refused = b.command(settle(p)).await;
    assert!(
        matches!(&refused, Err(PublicError::Invalid { message, .. })
            if message.contains("not an agent fn") && message.contains("step_cancel") && message.contains("step_set_output")),
        "{refused:?}"
    );
    assert!(matches!(
        b.guardian(C::CancelIntent(l.identity.clone()), Some(&l.capability))
            .await
            .unwrap(),
        R::CancelIntent(false)
    ));
    // A step that is not running is not finishing either.
    b.complete(
        journal(
            &b,
            &l,
            PayloadResult::Succeeded(decode_json(br#"{"value":1}"#).unwrap()),
        )
        .await,
    )
    .await
    .unwrap();
    let refused = b.command(settle(p)).await;
    assert!(
        matches!(&refused, Err(PublicError::Invalid { message, .. }) if message.contains("is succeeded, not finishing")),
        "{refused:?}"
    );
}

/// A run admitted before its plan moved on completes from what its attempt froze: the attempt
/// keeps no plan snapshot, only the revision it was admitted at, and later edits (a new step,
/// the running step's own tags) change nothing about its result.
async fn a_run_completes_from_its_attempt_after_the_plan_moved_on(
    b: Coordinator<Fake>,
    f: Fake,
    p: ProjectId,
) {
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let l = f.0.lock().unwrap()[0].clone();
    run_started(&b, &l).await;
    let attempt = l.identity.attempt;
    let (frozen, provenance): (Value, Value) = b
        .reads()
        .snapshot(move |sql| {
            let (request, provenance): (String, String) = sql.query_row(
                "SELECT request,provenance FROM attempts WHERE attempt_id=?1",
                [attempt.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok((
                serde_json::from_str(&request)?,
                serde_json::from_str(&provenance)?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(provenance["runtime"]["admitted_rev"], 2);
    assert!(
        provenance["runtime"].get("completion").is_none(),
        "{provenance}"
    );
    assert!(frozen["provenance"]["runtime"].get("completion").is_none());
    assert_eq!(frozen["declaration"]["run"], "fixture.submit");
    // The plan moves on while it runs: a step added, the running step retagged.
    let project = json!({"kind":"id","value":p});
    let CommandReply::Edit(edit) = b
        .command(request(json!({"command":"plan_edit","args":{"project":project,"reason":"later","ops":[
            {"op":"step.add","step":"later","spec":{"run":"fixture.echo","in":{"value":{"default":2}},"paused":true}},
            {"op":"step.update","step":"work","changes":{"tags":["while-running"]}}
        ]}})))
        .await
        .unwrap()
    else {
        panic!("an edit result")
    };
    assert_eq!(edit.rev, Revision(3));
    b.complete(
        journal(
            &b,
            &l,
            PayloadResult::Succeeded(decode_json(br#"{"value":1,"submitted":true}"#).unwrap()),
        )
        .await,
    )
    .await
    .unwrap();
    let CommandReply::Step(work) = b
        .command(request(
            json!({"command":"step_get","args":{"project":project,"step":"work","compact":true}}),
        ))
        .await
        .unwrap()
    else {
        panic!("a step")
    };
    let sluice_model::plan_rows::StepView::Compact(work) = work.step else {
        panic!("a compact step")
    };
    assert_eq!(work.status, StepStatus::Succeeded);
}

/// The edit pipeline's refusals: what a caller supplies empty is refused before anything is
/// prepared, an explicit stale revision is a conflict naming the current one, and a run's
/// callback may not edit another project.
async fn plan_edit_refuses_empty_supplies_and_a_stale_revision_before_preparing(
    b: Coordinator<Fake>,
    _f: Fake,
    p: ProjectId,
) {
    let project = json!({"kind":"id","value":p});
    let refused = |args: Value| {
        let b = b.clone();
        async move {
            b.command(request(json!({"command":"plan_edit","args":args})))
                .await
        }
    };
    let no_ops = refused(json!({"project":project,"ops":[],"reason":"none"}))
        .await
        .unwrap_err();
    assert_eq!(
        no_ops,
        PublicError::BadRequest {
            message: "ops: name at least one operation".into()
        }
    );
    let all = refused(
        json!({"project":project,"reason":"all","preview_scope":"all",
        "ops":[{"op":"step.update","step":"work","changes":{"doc":"x"}}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        all,
        PublicError::BadRequest {
            message: "preview_scope \"all\" needs dry_run: true".into()
        }
    );
    let stale = refused(json!({"project":project,"rev":1,"reason":"stale",
        "ops":[{"op":"step.update","step":"work","changes":{"doc":"x"}}]}))
    .await
    .unwrap_err();
    assert_eq!(
        stale,
        PublicError::Conflict {
            message: "plan is at rev 2".into(),
            current_rev: Some(Revision(2)),
        }
    );
    let empty_unit = b
        .command(request(json!({"command":"unit_update","args":{"project":project,"unit":"work","changes":{},"reason":"none"}})))
        .await
        .unwrap_err();
    assert_eq!(
        empty_unit,
        PublicError::BadRequest {
            message: "changes: name at least one step".into()
        }
    );
}

/// `verify` rebuilds every index row from the declarations and reports each stored one that
/// differs: a reference re-pointed by hand, a step's unit changed and an edge deleted.
async fn verify_reports_a_hand_corrupted_index_row(b: Coordinator<Fake>, _f: Fake, p: ProjectId) {
    let project = json!({"kind":"id","value":p});
    b.command(request(json!({"command":"plan_edit","args":{"project":project,"reason":"reader","ops":[
        {"op":"step.add","step":"reader","spec":{"run":"fixture.echo","in":{"value":{"source":"work/submitted"}}}}
    ]}})))
    .await
    .unwrap();
    let registry = Arc::new(b.catalog().clone());
    let index_problems = |problems: Vec<sluice_runtime::verify::Problem>| {
        problems
            .into_iter()
            .filter(|p| p.r#where.contains(": index#"))
            .map(|p| format!("{}: {}", p.r#where.split_once(": ").unwrap().1, p.message))
            .collect::<Vec<_>>()
    };
    let clean = sluice_runtime::verify::verify(b.reads(), registry.clone(), None)
        .await
        .unwrap();
    assert_eq!(index_problems(clean), Vec::<String>::new());
    b.writer()
        .write(sluice_store::RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE plan_refs SET source_port='value' WHERE project_id=?1 AND consumer_id='reader'",
                [p.to_string()],
            )?;
            tx.sql().execute(
                "UPDATE steps SET unit='elsewhere' WHERE project_id=?1 AND step_id='reader'",
                [p.to_string()],
            )?;
            tx.sql().execute(
                "DELETE FROM plan_edges WHERE project_id=?1 AND target_step='reader'",
                [p.to_string()],
            )?;
            tx.changed(Some(p), "plan");
            Ok(())
        })
        .await
        .unwrap();
    let found = index_problems(
        sluice_runtime::verify::verify(b.reads(), registry, None)
            .await
            .unwrap(),
    );
    assert_eq!(
        found,
        [
            "index#steps.reader.unit: stored unit elsewhere differs from the declaration's reader",
            "index#plan_refs.step.reader.in.value[0]: declared reference missing from the index: binding step work/submitted",
            "index#plan_refs.step.reader.in.value[0]: stored reference the declarations do not make: binding step work/value",
            "index#plan_edges.reader: declared edge missing from the index: data reader after work",
        ]
    );
}

async fn fresh_project(b: &Coordinator<Fake>, n: usize) -> ProjectId {
    let CommandReply::Project(p) = b.command(request(json!({"command":"project_create","args":{"name":format!("case-{n}"),"description":"","resources":{}}}))).await.unwrap() else { panic!("project") };
    b.command(request(json!({"command":"plan_edit","args":{"project":{"kind":"id","value":p.project_id},"ops":[{"op":"step.add","step":"work","spec":{"run":"fixture.submit","in":{"value":{"default":1}},"outputs":{"submitted":"boolean"}}}],"reason":"fixture"}}))).await.unwrap();
    p.project_id
}

#[tokio::test]
async fn messages_edits_and_index_refusals() {
    let (_home, b, f, mut p) = setup().await;
    ask_say_and_reply_reach_every_recipient_on_derived_threads(b.clone(), f.clone(), p).await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
    p = fresh_project(&b, 1).await;
    a_missing_unknown_or_removed_recipient_is_invalid_and_stores_nothing(b.clone(), f.clone(), p)
        .await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
    p = fresh_project(&b, 2).await;
    board_set_and_plan_edits_warn_for_the_steps_the_board_loses(b.clone(), f.clone(), p).await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
    p = fresh_project(&b, 3).await;
    plan_edit_refuses_empty_supplies_and_a_stale_revision_before_preparing(b.clone(), f.clone(), p)
        .await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
    p = fresh_project(&b, 4).await;
    verify_reports_a_hand_corrupted_index_row(b.clone(), f.clone(), p).await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
}

#[tokio::test]
async fn run_identity_delivery_and_replay() {
    let (_home, b, f, mut p) = setup().await;
    claim_replay_is_same_identity_only_and_start_consumes_backlog_once(b.clone(), f.clone(), p)
        .await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
    p = fresh_project(&b, 1).await;
    receipts_follow_the_step_from_pending_through_its_run_to_done(b.clone(), f.clone(), p).await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
    p = fresh_project(&b, 2).await;
    callbacks_deduplicate_atomically_and_completion_replay_does_not_release_replacement(
        b.clone(),
        f.clone(),
        p,
    )
    .await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
    p = fresh_project(&b, 3).await;
    cancellation_prevents_claim_and_bad_capability_cannot_read_run(b.clone(), f.clone(), p).await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
}

#[tokio::test]
async fn submissions_completion_and_settlement() {
    let (_home, b, f, mut p) = setup().await;
    current_reserved_run_accepts_submissions_before_claim_and_after_feedback(
        b.clone(),
        f.clone(),
        p,
    )
    .await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
    p = fresh_project(&b, 1).await;
    terminal_callbacks_replay_exact_replies_and_refuse_new_mutations(b.clone(), f.clone(), p).await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
    p = fresh_project(&b, 2).await;
    a_running_step_that_has_submitted_is_finishing_in_status_step_context_and_call_status(
        b.clone(),
        f.clone(),
        p,
    )
    .await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
    p = fresh_project(&b, 3).await;
    step_settle_refuses_a_fn_that_composes_its_agent_with_the_way_by_hand(b.clone(), f.clone(), p)
        .await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
    p = fresh_project(&b, 4).await;
    a_run_completes_from_its_attempt_after_the_plan_moved_on(b.clone(), f.clone(), p).await;
    b.release_scheduler("s".into()).await.unwrap();
    f.0.lock().unwrap().clear();
}

#[tokio::test]
async fn execution_status_preserves_frozen_limits_when_configuration_breaks() {
    let (home, broker, fake, project) = setup().await;
    broker
        .acquire_scheduler("budget-status".into())
        .await
        .unwrap();
    assert_eq!(
        reconcile_project(&broker, project, "budget-status")
            .await
            .unwrap(),
        1
    );
    let launch = fake.launched(1).await.remove(0);
    std::fs::write(home.path().join("config.json"), "{broken").unwrap();
    let status = data_of(&broker, json!({"command":"status","args":{"project":{"kind":"id","value":project},"all":true,"selection":{"steps":null,"tags":null}}})).await;
    assert_eq!(status["steps"]["work"]["limits"], launch.limits.shown());
    assert_eq!(
        status["steps"]["work"]["run_resources"][0]["run"],
        json!(launch.identity.run)
    );
    assert!(status["execution"]["error"].is_object());
    let rejected = broker.command(request(json!({"command":"plan_edit","args":{"project":{"kind":"id","value":project},"ops":[{"op":"step.add","step":"over","spec":{"run":"fixture.submit","in":{"value":{"default":1}},"limits":{"memory":"1TiB"}}}],"reason":"invalid budget"}}))).await;
    assert!(rejected.is_err());
    std::fs::write(home.path().join("config.json"), "{}").unwrap();
    let rejected = broker.command(request(json!({"command":"plan_edit","args":{"project":{"kind":"id","value":project},"ops":[{"op":"step.add","step":"over","spec":{"run":"fixture.submit","in":{"value":{"default":1}},"limits":{"memory":"1024TiB"}}}],"reason":"over ceiling"}}))).await;
    assert!(
        matches!(rejected, Err(PublicError::Invalid { .. })),
        "{rejected:?}"
    );
}

#[tokio::test]
async fn terminal_guardian_retains_budget_until_service_cleanup_is_proven() {
    let (_home, broker, fake, project) = setup().await;
    fake.2.store(true, std::sync::atomic::Ordering::SeqCst);
    broker.acquire_scheduler("resources".into()).await.unwrap();
    reconcile_project(&broker, project, "resources")
        .await
        .unwrap();
    let launch = fake.0.lock().unwrap()[0].clone();
    broker
        .guardian(C::Claim(guardian(&launch)), Some(&launch.capability))
        .await
        .unwrap();
    broker
        .complete(CompletionJournal {
            protocol: 1,
            limits: launch.limits,
            guardian_memory_bytes: launch.policy.guardian_memory_bytes,
            usage: Some(Default::default()),
            identity: launch.identity.clone(),
            completion_id: "terminal-payload".into(),
            result: PayloadResult::Failed(PublicError::FnFailure {
                message: "fixture complete".into(),
            }),
            starts: vec![],
            exits: vec![],
            cleanup: vec![CleanupEvidence {
                cgroup: "fake/payload".into(),
                empty: true,
                escalated: false,
            }],
            submissions: JsonMap::default(),
            submission_version: None,
            delivery_acks: vec![],
        })
        .await
        .unwrap();
    assert_eq!(live_attempts(&broker).await, 1);
    let drain = sluice_runtime::drain::status(broker.reads()).await.unwrap();
    assert!(
        drain
            .blockers
            .iter()
            .any(|blocker| blocker.identity == launch.identity.run.to_string())
    );
    broker.reap_terminal(false).await.unwrap();
    assert_eq!(live_attempts(&broker).await, 1);
    broker.adopt().await.unwrap();
    assert_eq!(live_attempts(&broker).await, 0);
    assert!(
        sluice_runtime::drain::status(broker.reads())
            .await
            .unwrap()
            .blockers
            .is_empty()
    );
    broker.writer().shutdown().await.unwrap();
}

/// Attempts whose service is not yet proven gone.
async fn live_attempts(b: &Coordinator<Fake>) -> u64 {
    b.reads()
        .snapshot(sluice_store::convert::live_work)
        .await
        .unwrap()
        .attempts
}
fn finite_home() -> home::ScratchHome {
    let home = home::ScratchHome::new().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        json!({"execution":{
            "defaults":{"memory":"64MiB","cpus":0.1,"tasks":64},
            "guardian_memory":"64MiB"
        }})
        .to_string(),
    )
    .unwrap();
    home
}

#[tokio::test]
async fn delayed_os_launch_cannot_start_after_adoption_finishes_attempt() {
    use sluice_process::locks::RunFence;
    use sluice_runtime::execution::OsHost;
    let (home, broker, fake, project) = setup().await;
    fake.2.store(true, std::sync::atomic::Ordering::SeqCst);
    broker
        .acquire_scheduler("launch-fence".into())
        .await
        .unwrap();
    reconcile_project(&broker, project, "launch-fence")
        .await
        .unwrap();
    let launch = fake.0.lock().unwrap()[0].clone();
    let fence = RunFence::acquire(home.path(), launch.identity.run)
        .await
        .unwrap();
    let host = OsHost {
        home: home.path().into(),
        program: std::env::current_exe().unwrap(),
    };
    let pending = host.launch(launch.clone());
    tokio::pin!(pending);
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(pending.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    broker
        .complete(CompletionJournal {
            protocol: 1,
            limits: launch.limits,
            guardian_memory_bytes: launch.policy.guardian_memory_bytes,
            usage: None,
            identity: launch.identity.clone(),
            completion_id: "adopted-before-delayed-launch".into(),
            result: PayloadResult::Lost("start not observed".into()),
            starts: vec![],
            exits: vec![],
            cleanup: vec![CleanupEvidence {
                cgroup: "fake/payload".into(),
                empty: true,
                escalated: false,
            }],
            submissions: JsonMap::default(),
            submission_version: None,
            delivery_acks: vec![],
        })
        .await
        .unwrap();
    // Even a terminal run cannot release its reservation while a start helper owns the fence.
    broker.adopt().await.unwrap();
    assert_eq!(live_attempts(&broker).await, 1);
    drop(fence);
    let outcome = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome,
        LaunchOutcome::Refused(PublicError::Conflict { .. })
    ));
    assert!(
        !home
            .path()
            .join("runs")
            .join(launch.identity.run.to_string())
            .join("runtime.json")
            .exists()
    );
    broker.adopt().await.unwrap();
    assert_eq!(live_attempts(&broker).await, 0);
}

/// Bug: a run's task-limit hit (EAGAIN in its build) reached nobody.
#[tokio::test]
async fn a_limit_hit_reaches_the_owner_once() {
    let home = home::ScratchHome::new().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        json!({"execution":{"defaults":{"memory":"48MiB"},"ceilings":{"memory":"48MiB"},"total":{"memory":"128MiB","tasks":4096},"guardian_memory":"16MiB"}}).to_string(),
    )
    .unwrap();
    let fake = Fake::default();
    let b = Coordinator::open(home.path().into(), Catalog::fixtures(), fake.clone())
        .await
        .unwrap();
    let CommandReply::Project(p)=b.command(request(json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":{},"author":"test"}}))).await.unwrap()else{panic!("project")};
    let p = p.project_id;
    let selector = json!({"kind":"id","value":p});
    b.command(request(
        json!({"command":"plan_edit","args":{"project":selector,"rev":1,"ops":[
        {"op":"step.add","step":"a","spec":{"run":"fixture.submit","in":{"value":{"default":1}},"outputs":{"submitted":"boolean"},"limits":{"tasks":32}}}
    ],"start":true,"dry_run":false,"reason":"test","author":"@lead","me":"@lead"}}),
    ))
    .await
    .unwrap();
    b.acquire_scheduler("limits".into()).await.unwrap();
    assert_eq!(reconcile_project(&b, p, "limits").await.unwrap(), 1);
    let a = fake.launched(1).await.remove(0);
    assert_eq!((a.limits.tasks, a.limits.cpu_millis), (Some(32), None));
    let status = data_of(&b, json!({"command":"status","args":{"project":selector,"all":true,"selection":{"steps":null,"tags":null}}})).await;
    // The home's slice has a CPU quota by default; a run has none of its own.
    assert!(
        status["execution"]["total"]["cpu_millis"].is_u64(),
        "{status}"
    );
    assert_eq!(status["steps"]["a"]["limits"]["cpu_millis"], "unlimited");

    // A live run's failed forks (pids.events) reach its unit's owner and the step itself,
    // once, however often it is sampled and when it then finishes.
    run_started(&b, &a).await;
    let hit = sluice_model::RunUsage {
        pids_limit_hits: 3,
        ..Default::default()
    };
    fake.3.lock().unwrap().insert(a.identity.run, hit);
    assert_eq!(b.sample_limits().await.unwrap(), 1);
    assert_eq!(b.sample_limits().await.unwrap(), 0);
    let mut done = journal(&b, &a, PayloadResult::Succeeded(JsonMap::default())).await;
    done.usage = Some(hit);
    b.complete(done).await.unwrap();
    let notices: Vec<(String, Value)> = b
        .reads()
        .snapshot(move |sql| {
            let mut statement = sql.prepare("SELECT \"to\",data FROM messages WHERE project_id=?1 AND \"from\"='sluice' AND json_extract(data,'$.execution_limit') IS NOT NULL ORDER BY id")?;
            let rows = statement
                .query_map([p.to_string()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows.into_iter().map(|(to, data)| (to, serde_json::from_str(&data).unwrap())).collect())
        })
        .await
        .unwrap();
    let to: Vec<&str> = notices.iter().map(|(to, _)| to.as_str()).collect();
    assert_eq!(to, ["@lead", "a"], "{notices:?}");
    let notice = &notices[0].1["execution_limit"];
    assert_eq!(notice["limit"], "tasks");
    assert_eq!(notice["value"], 32);
    assert_eq!(notice["observed"]["failed_forks"], 3);
    assert_eq!(notice["outcome"], "running");
    assert_eq!(notice["run"], json!(a.identity.run));
    assert_eq!(notice["step"], "a");
    // The owner's `next` wakes on it.
    let CommandReply::Next(next) = b.command(request(json!({"command":"next","args":{"projects":[selector],"since_seq":0,"me":"@lead","timeout_seconds":0,"all":false,"settle_seconds":0,"settle_max_seconds":0,"settles":"none"}}))).await.unwrap() else { panic!("next") };
    assert!(
        next.records.iter().any(
            |r| serde_json::to_value(r).unwrap()["data"]["execution_limit"]["limit"] == "tasks"
        ),
        "{next:?}"
    );
}

/// Bug: reclaimable build cache reaching memory.max warned that a healthy run might die.
#[tokio::test]
async fn cache_reclaim_does_not_warn_of_memory_exhaustion() {
    use sluice_model::limits::RunMemory;
    let (home, b, fake, p) = setup().await;
    let triggers = [
        "work",
        "working",
        "psi",
        "oom",
        "kill",
        "configured_working",
        "configured_psi",
    ];
    let original_config = std::fs::read(home.path().join("config.json")).unwrap();
    let selector = json!({"kind":"id","value":p});
    let ops: Vec<_> = triggers[1..].iter().map(|step| json!({"op":"step.add","step":step,
        "spec":{"run":"fixture.submit","in":{"value":{"default":1}},"outputs":{"submitted":"boolean"}}})).collect();
    b.command(request(json!({"command":"plan_edit","args":{"project":selector,"rev":2,"ops":ops,"start":true,"dry_run":false,"reason":"test","author":"test"}}))).await.unwrap();
    b.acquire_scheduler("memory-notice".into()).await.unwrap();
    reconcile_project(&b, p, "memory-notice").await.unwrap();
    let launches = fake.launched(triggers.len()).await;
    for trigger in triggers {
        let launch = launches
            .iter()
            .find(|l| l.identity.step.as_ref().unwrap().as_str() == trigger)
            .unwrap()
            .clone();
        run_started(&b, &launch).await;
        let run = launch.identity.run;
        let cap = launch.limits.memory_max_bytes.unwrap();
        let mut usage = sluice_model::RunUsage {
            memory_peak_bytes: cap,
            memory_max_hits: 4438,
            ..Default::default()
        };
        fake.3.lock().unwrap().insert(run, usage);
        // Unknown composition must not turn reclaim counts or an old peak into a warning.
        assert_eq!(b.sample_limits().await.unwrap(), 0);
        let mut memory = RunMemory {
            current_bytes: cap,
            peak_bytes: cap,
            working_bytes: Some(cap / 8),
            file_cache_bytes: Some(cap * 7 / 8),
            pressure_some_avg10: Some(0.0),
            oom_events: 0,
        };
        fake.5.lock().unwrap().insert(run, memory);
        assert_eq!(
            b.sample_limits().await.unwrap(),
            0,
            "reclaiming file cache must not warn that the run may be killed"
        );
        *fake.4.lock().unwrap() = Some(sluice_runtime::headroom::HostReading {
            available_bytes: 16 << 30,
            ..Default::default()
        });
        reconcile_project(&b, p, "memory-notice").await.unwrap();
        let selector = json!({"kind":"id","value":p});
        let status = data_of(&b, json!({"command":"status","args":{"project":selector,"all":true,"selection":{"steps":null,"tags":null}}})).await;
        let shown = &status["steps"][trigger]["run_resources"][0]["memory"];
        assert_eq!(shown["cap_bytes"], cap);
        assert_eq!(shown["current_bytes"], cap);
        assert_eq!(shown["peak_bytes"], cap);
        assert_eq!(shown["working_bytes"], cap / 8);
        assert_eq!(shown["working_peak_bytes"], cap / 8);
        assert_eq!(shown["file_cache_bytes"], cap * 7 / 8);
        match trigger {
            "work" => {}
            "working" => memory.working_bytes = Some(cap * 95 / 100),
            "psi" => memory.pressure_some_avg10 = Some(15.0),
            "oom" => memory.oom_events = 1,
            "kill" => {
                usage.oom_kills = 1;
            }
            "configured_working" => memory.working_bytes = Some(cap * 3 / 5),
            "configured_psi" => memory.pressure_some_avg10 = Some(5.0),
            _ => unreachable!(),
        }
        fake.3.lock().unwrap().insert(run, usage);
        fake.5.lock().unwrap().insert(run, memory);
        if trigger.starts_with("configured_") {
            assert_eq!(b.sample_limits().await.unwrap(), 0, "{trigger}");
            let path = home.path().join("config.json");
            let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            config["execution"]["memory_notice_working_percent"] = json!(50);
            config["execution"]["memory_notice_pressure_percent"] = json!(4);
            std::fs::write(path, config.to_string()).unwrap();
        }
        if trigger == "kill" {
            fake.5.lock().unwrap().remove(&run);
            // Bad admission configuration must not suppress kernel OOM evidence.
            std::fs::write(home.path().join("config.json"), "invalid json").unwrap();
        }
        assert_eq!(
            b.sample_limits().await.unwrap(),
            usize::from(trigger != "work"),
            "{trigger}"
        );
        assert_eq!(
            b.sample_limits().await.unwrap(),
            0,
            "notice repeats: {trigger}"
        );
        std::fs::write(home.path().join("config.json"), &original_config).unwrap();
        let mut done = journal(&b, &launch, PayloadResult::Succeeded(JsonMap::default())).await;
        done.usage = Some(usage);
        b.complete(done.clone()).await.unwrap();
        let recorded = reported_usage(&b, &done).await;
        assert_eq!(recorded["memory_max_hits"], 4438);
        assert_eq!(
            recorded["memory_working_peak_bytes"],
            memory.working_bytes.unwrap()
        );
        assert_eq!(recorded["memory_peak_bytes"], cap);
        let CommandReply::Records(page) = b.command(request(json!({"command":"log_read","args":{"project":selector,"since_seq":null,"kinds":["message"],"threads":null,"limit":100}}))).await.unwrap() else { panic!("records") };
        let records = serde_json::to_value(page.records).unwrap();
        let notices: Vec<_> = records
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| {
                r["data"]["execution_limit"]["limit"] == "memory"
                    && r["data"]["execution_limit"]["run"] == run.to_string()
            })
            .collect();
        assert_eq!(
            notices.is_empty(),
            trigger == "work",
            "{trigger}: {records}"
        );
        for notice in notices {
            let observed = &notice["data"]["execution_limit"]["observed"];
            if trigger == "kill" {
                assert_eq!(observed["oom_kills"], 1);
            } else {
                assert_eq!(observed["working_bytes"], memory.working_bytes.unwrap());
                assert_eq!(observed["file_cache_bytes"], cap * 7 / 8);
                assert_eq!(observed["max_events"], 4438);
            }
        }
    }
}

/// Bugs: admission summed every live run's reservation against the total, so steps queued for
/// 20 minutes with the host half idle (2026-10-20); before that, uncapped runs were admitted
/// with about 1 GB free and the host in reclaim (2026-10-19). A run now starts on measured
/// memory: MemAvailable less the reserve must cover its charge, PSI must be under its limit,
/// and the live runs' measured memory plus the charge must fit the total. Runs inside their
/// warm-up count at the charge, so a burst never passes on one stale reading. Status says which
/// check holds a step, with its numbers, and shows each run's cap, memory and peak.
#[tokio::test]
async fn admission_counts_measured_memory_and_new_runs_at_their_charge() {
    let home = home::ScratchHome::new().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        json!({"execution":{
            "defaults":{"memory":"256MiB"},
            "ceilings":{"memory":"512MiB"},
            "total":{"memory":"1GiB"},
            "guardian_memory":"16MiB",
            "host_reserve":"1GiB",
            "memory_pressure_max":10,
            "admission_charge":"64MiB",
            "admission_warmup_s":30
        }})
        .to_string(),
    )
    .unwrap();
    let fake = Fake::default();
    let host = |available: u64, pressure: f64| {
        *fake.4.lock().unwrap() = Some(sluice_runtime::headroom::HostReading {
            available_bytes: available,
            memory_pressure_some_avg10: Some(pressure),
            ..Default::default()
        });
    };
    let warm_up = || {
        fake.6.fetch_add(31, std::sync::atomic::Ordering::SeqCst);
    };
    // Five charges above the reserve.
    host((1 << 30) + (320 << 20), 0.0);
    let b = Coordinator::open(home.path().into(), Catalog::fixtures(), fake.clone())
        .await
        .unwrap();
    let CommandReply::Project(p)=b.command(request(json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":{},"author":"test"}}))).await.unwrap()else{panic!("project")};
    let p = p.project_id;
    let selector = json!({"kind":"id","value":p});
    let ops: Vec<Value> = (0..20).map(|n| json!({"op":"step.add","step":format!("s{n:02}"),"spec":{"run":"fixture.submit","in":{"value":{"default":n}},"outputs":{"submitted":"boolean"}}})).collect();
    b.command(request(json!({"command":"plan_edit","args":{"project":selector,"ops":ops,"start":true,"dry_run":false,"reason":"test","author":"@lead","me":"@lead"}}))).await.unwrap();
    let status = async || {
        data_of(&b, json!({"command":"status","args":{"project":selector,"all":true,"selection":{"steps":null,"tags":null}}})).await
    };
    let held = async || {
        let status = status().await;
        let mut held: Vec<String> = status["steps"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(|step| step["waiting"].as_array())
            .flatten()
            .filter_map(|reason| reason.as_str().map(str::to_owned))
            .collect();
        held.sort();
        held.dedup();
        held
    };
    b.acquire_scheduler("memory".into()).await.unwrap();

    // A burst of 20 with room for five charges starts five, and the same reading starts no more.
    assert_eq!(reconcile_project(&b, p, "memory").await.unwrap(), 5);
    assert_eq!(reconcile_project(&b, p, "memory").await.unwrap(), 0);
    let first = fake.launched(5).await;
    // A run's cap is the default and reserves nothing.
    assert!(
        first
            .iter()
            .all(|l| l.limits.memory_max_bytes == Some(256 << 20))
    );
    assert_eq!(
        held().await,
        ["held: host memory (available 1.3 GiB, reserve 1 GiB, starting 320 MiB, need 64 MiB)"]
    );
    // Past the warm-up each counts at its measured memory, which MemAvailable now shows.
    for launch in &first {
        run_started(&b, launch).await;
        fake.5.lock().unwrap().insert(
            launch.identity.run,
            sluice_model::limits::RunMemory {
                current_bytes: 32 << 20,
                peak_bytes: 48 << 20,
                ..Default::default()
            },
        );
    }
    warm_up();
    host((1 << 30) + (160 << 20), 0.0);
    assert_eq!(reconcile_project(&b, p, "memory").await.unwrap(), 2);
    assert_eq!(fake.launched(7).await.len(), 7);
    let status = status().await;
    let run = &status["steps"][first[0].identity.step.as_ref().unwrap().as_str()]["run_resources"]
        [0]["memory"];
    assert_eq!(
        *run,
        json!({"cap_bytes": 256 << 20, "current_bytes": 32 << 20, "peak_bytes": 48 << 20, "working_peak_bytes": null, "working_bytes": null, "file_cache_bytes": null, "pressure_some_avg10": null}),
        "{status}"
    );
    assert_eq!(
        status["execution"]["memory"]["measured_bytes"],
        json!((5 * 32 + 2 * 64) << 20),
        "{status}"
    );

    // Low MemAvailable holds, says so once to the owner, and admits when it recovers.
    warm_up();
    host(512 << 20, 0.0);
    assert_eq!(reconcile_project(&b, p, "memory").await.unwrap(), 0);
    assert_eq!(
        held().await,
        ["held: host memory (available 512 MiB, reserve 1 GiB, need 64 MiB)"]
    );
    let notices = b
        .reads()
        .snapshot(move |sql| {
            let mut statement = sql.prepare("SELECT DISTINCT \"to\" FROM messages WHERE project_id=?1 AND \"from\"='sluice' AND json_extract(data,'$.execution_limit.limit')='host_memory'")?;
            let rows = statement
                .query_map([p.to_string()], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert_eq!(notices, ["@lead"]);
    host((1 << 30) + (64 << 20), 0.0);
    assert_eq!(reconcile_project(&b, p, "memory").await.unwrap(), 1);
    assert_eq!(fake.launched(8).await.len(), 8);

    // High memory pressure holds however much is free.
    warm_up();
    host(8 << 30, 25.0);
    assert_eq!(reconcile_project(&b, p, "memory").await.unwrap(), 0);
    assert_eq!(
        held().await,
        ["held: memory pressure (some avg10 25.0%, limit 10%)"]
    );

    // The live runs' measured memory leaves no room in the total: 5 × 170 MiB measured, and
    // three runs without a reading at their charge.
    host(8 << 30, 1.0);
    for launch in &first {
        fake.5.lock().unwrap().insert(
            launch.identity.run,
            sluice_model::limits::RunMemory {
                current_bytes: 170 << 20,
                peak_bytes: 170 << 20,
                working_bytes: Some(170 << 20),
                ..Default::default()
            },
        );
    }
    assert_eq!(reconcile_project(&b, p, "memory").await.unwrap(), 0);
    assert_eq!(
        held().await,
        ["held: pool memory (measured 1 GiB + 64 MiB > 1 GiB)"]
    );
    // Working memory shrinks while file cache stays charged: (5 × 100 + 3 × 64) MiB
    // measured leaves room for five more.
    for launch in &first {
        fake.5.lock().unwrap().insert(
            launch.identity.run,
            sluice_model::limits::RunMemory {
                current_bytes: 170 << 20,
                peak_bytes: 170 << 20,
                working_bytes: Some(100 << 20),
                file_cache_bytes: Some(70 << 20),
                ..Default::default()
            },
        );
    }
    assert_eq!(reconcile_project(&b, p, "memory").await.unwrap(), 5);
    assert_eq!(fake.launched(13).await.len(), 13);
    let sample = data_of(&b, json!({"command":"status","args":{"project":selector,"all":true,"selection":{"steps":null,"tags":null}}})).await;
    let measured = &sample["steps"][first[0].identity.step.as_ref().unwrap().as_str()]["run_resources"]
        [0]["memory"];
    assert_eq!(measured["working_peak_bytes"], 170 << 20);
    assert_eq!(
        held().await,
        ["held: pool memory (measured 1012 MiB + 64 MiB > 1 GiB)"]
    );
}

/// Bug (incident 2026-10-20): turbo reached a 1-minute load of 590 on 32 CPUs with 36 GB free,
/// because admission checked only memory and kept starting runs, about 59 at once. A run now
/// also waits while CPU pressure or the load per CPU is at its limit, and while either is over
/// half its limit at most `warmup_starts` runs start within one warm-up. A burst of 40 ready
/// steps on a loaded host is held with the readings' numbers and the owner is told once.
#[tokio::test]
async fn admission_holds_a_burst_on_cpu_pressure_load_and_recent_starts() {
    let home = home::ScratchHome::new().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        json!({"execution":{
            "defaults":{"memory":"256MiB"},
            "ceilings":{"memory":"512MiB"},
            "total":{"memory":"4GiB"},
            "guardian_memory":"16MiB",
            "host_reserve":"1GiB",
            "admission_charge":"64MiB",
            "admission_warmup_s":30,
            "cpu_pressure_max":30,
            "load_per_cpu_max":1.5,
            "warmup_starts":2
        }})
        .to_string(),
    )
    .unwrap();
    let fake = Fake::default();
    let host = |pressure: f64, load: f64| {
        *fake.4.lock().unwrap() = Some(sluice_runtime::headroom::HostReading {
            available_bytes: 8 << 30,
            memory_pressure_some_avg10: Some(0.0),
            cpu_pressure_some_avg10: Some(pressure),
            load_avg1: Some(load),
            cpus: Some(32),
        });
    };
    host(74.0, 590.0);
    let b = Coordinator::open(home.path().into(), Catalog::fixtures(), fake.clone())
        .await
        .unwrap();
    let CommandReply::Project(p)=b.command(request(json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":{},"author":"test"}}))).await.unwrap()else{panic!("project")};
    let p = p.project_id;
    let selector = json!({"kind":"id","value":p});
    let ops: Vec<Value> = (0..40).map(|n| json!({"op":"step.add","step":format!("s{n:02}"),"spec":{"run":"fixture.submit","in":{"value":{"default":n}},"outputs":{"submitted":"boolean"}}})).collect();
    b.command(request(json!({"command":"plan_edit","args":{"project":selector,"ops":ops,"start":true,"dry_run":false,"reason":"test","author":"@lead","me":"@lead"}}))).await.unwrap();
    let status = async || {
        data_of(&b, json!({"command":"status","args":{"project":selector,"all":true,"selection":{"steps":null,"tags":null}}})).await
    };
    let held = async || {
        let status = status().await;
        let mut held: Vec<String> = status["steps"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(|step| step["waiting"].as_array())
            .flatten()
            .filter_map(|reason| reason.as_str().map(str::to_owned))
            .collect();
        held.sort();
        held.dedup();
        held
    };
    b.acquire_scheduler("cpu".into()).await.unwrap();

    // The incident's host: CPU pressure over its limit holds all 40, and the owner hears once.
    assert_eq!(reconcile_project(&b, p, "cpu").await.unwrap(), 0);
    assert_eq!(
        held().await,
        ["held: cpu pressure (some avg10 74.0%, limit 30%)"]
    );
    let notices = b
        .reads()
        .snapshot(move |sql| {
            let mut statement = sql.prepare("SELECT DISTINCT \"to\" FROM messages WHERE project_id=?1 AND \"from\"='sluice' AND json_extract(data,'$.execution_limit.limit')='host_cpu'")?;
            let rows = statement
                .query_map([p.to_string()], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert_eq!(notices, ["@lead"]);
    // The load per CPU holds on its own.
    host(5.0, 590.0);
    assert_eq!(reconcile_project(&b, p, "cpu").await.unwrap(), 0);
    assert_eq!(
        held().await,
        ["held: cpu load (1-min load 590.0 on 32 CPUs, 18.44 per CPU, limit 1.5)"]
    );

    // Over half a limit, two start and the rest wait out the warm-up on the same reading.
    host(20.0, 16.0);
    assert_eq!(reconcile_project(&b, p, "cpu").await.unwrap(), 2);
    assert_eq!(reconcile_project(&b, p, "cpu").await.unwrap(), 0);
    assert_eq!(fake.launched(2).await.len(), 2);
    assert_eq!(
        held().await,
        [
            "held: cpu warm-up (2 runs started within 30 s, limit 2 while cpu pressure 20.0%, over half of 30%)"
        ]
    );
    host(5.0, 32.0);
    assert_eq!(reconcile_project(&b, p, "cpu").await.unwrap(), 0);
    assert_eq!(
        held().await,
        [
            "held: cpu warm-up (2 runs started within 30 s, limit 2 while load 1.00 per CPU, over half of 1.5)"
        ]
    );
    let shown = &status().await["execution"]["cpu"];
    assert_eq!(
        *shown,
        json!({"pressure_some_avg10": 5.0, "load_avg1": 32.0, "cpus": 32, "load_per_cpu": 1.0,
            "starting_runs": 2, "pressure_max": 30.0, "load_per_cpu_max": 1.5,
            "warmup_starts": 2, "cpu_weight": 20}),
    );
    // Past the warm-up two more start.
    fake.6.fetch_add(31, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(reconcile_project(&b, p, "cpu").await.unwrap(), 2);
    assert_eq!(fake.launched(4).await.len(), 4);
    // A quiet host starts the rest at once.
    host(1.0, 4.0);
    assert_eq!(reconcile_project(&b, p, "cpu").await.unwrap(), 36);
    assert_eq!(fake.launched(40).await.len(), 40);
}

/// Bug (2026-10-20): a total above the host's effective memory left the coordinator serving
/// and every step queued behind a generic hold. It now refuses to start, naming the setting.
#[tokio::test]
async fn the_coordinator_does_not_start_on_an_invalid_execution_config() {
    let home = home::ScratchHome::new().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        json!({"execution":{"total":{"memory":"1024TiB"}}}).to_string(),
    )
    .unwrap();
    match Coordinator::open(home.path().into(), Catalog::fixtures(), Fake::default()).await {
        Err(PublicError::Invalid { message, .. }) => assert!(
            message.starts_with("config.json: execution.total.memory (1024 TiB) exceeds"),
            "{message}"
        ),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("opened"),
    }
}

/// Bug: a step that needed more than the per-run ceiling could not run at all, and nothing
/// stopped one from quietly asking for the home's whole total. A cap above the ceiling is refused
/// when the step is added unless the step sets `memory_above_ceiling` and its project's limits
/// allow it; it never exceeds the home total, and the run gets that cap.
#[tokio::test]
async fn memory_above_the_ceiling_needs_the_step_flag_and_the_project_allowance() {
    let home = home::ScratchHome::new().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        json!({"execution":{
            "defaults":{"memory":"256MiB"},
            "ceilings":{"memory":"512MiB"},
            "total":{"memory":"1GiB"},
            "guardian_memory":"16MiB"
        }})
        .to_string(),
    )
    .unwrap();
    let fake = Fake::default();
    let b = Coordinator::open(home.path().into(), Catalog::fixtures(), fake.clone())
        .await
        .unwrap();
    let CommandReply::Project(p)=b.command(request(json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":{},"author":"test"}}))).await.unwrap()else{panic!("project")};
    let p = p.project_id;
    let selector = json!({"kind":"id","value":p});
    let add = |limits: Value| {
        request(
            json!({"command":"plan_edit","args":{"project":selector,"ops":[
            {"op":"step.add","step":"big","spec":{"run":"fixture.submit","in":{"value":{"default":1}},"outputs":{"submitted":"boolean"},"limits":limits}}
        ],"start":true,"dry_run":false,"reason":"test","author":"@lead","me":"@lead"}}),
        )
    };
    let refused = async |limits: Value| match b.command(add(limits)).await {
        Err(PublicError::Invalid { message, .. }) => message,
        other => panic!("{other:?}"),
    };
    let message = refused(json!({"memory":"768MiB"})).await;
    assert!(
        message.contains("768 MiB is above the per-run memory ceiling (512 MiB)")
            && message.contains("memory_above_ceiling"),
        "{message}"
    );
    let message = refused(json!({"memory":"768MiB","memory_above_ceiling":true})).await;
    assert!(
        message.contains("this project does not allow that"),
        "{message}"
    );
    b.command(request(json!({"command":"project_update","args":{"project":selector,"limits":{"memory_above_ceiling":true},"reason":"one big build","author":"owner"}}))).await.unwrap();
    // Still explicit per step, and never above the home total.
    let message = refused(json!({"memory":"768MiB"})).await;
    assert!(message.contains("memory_above_ceiling"), "{message}");
    let message = refused(json!({"memory":"1GiB","memory_above_ceiling":true})).await;
    assert!(
        message.contains("exceeds the home memory total (1 GiB)"),
        "{message}"
    );
    b.command(add(json!({"memory":"768MiB","memory_above_ceiling":true})))
        .await
        .unwrap();
    b.acquire_scheduler("big".into()).await.unwrap();
    assert_eq!(reconcile_project(&b, p, "big").await.unwrap(), 1);
    let launch = fake.launched(1).await.remove(0);
    assert_eq!(launch.limits.memory_max_bytes, Some(768 << 20));
}
