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
#[tokio::test]
async fn claim_replay_is_same_identity_only_and_start_consumes_backlog_once() {
    let (_home, b, f, p) = setup().await;
    b.command(request(json!({"command":"say","args":{"project":json!({"kind":"id","value":p}),"body":"feedback","to":"work","data":null,"run":null}}))).await.unwrap();
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
/// One ask, say or reply through the coordinator's command dispatch: its receipt.
async fn speak(
    b: &Coordinator<Fake>,
    p: ProjectId,
    verb: &str,
    args: Value,
) -> Result<MessageReceipt, PublicError> {
    let mut args = args;
    args["project"] = json!({"kind":"id","value":p});
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
#[tokio::test]
async fn ask_say_and_reply_reach_every_recipient_on_derived_threads() {
    let (_home, b, _f, p) = setup().await;
    // The orchestrator (no run identity) and the owner (the dashboard) speak to every
    // recipient kind: a step of the plan, the orchestrator and the owner.
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
    assert_eq!(
        (ask_owner.thread.as_str(), ask_owner.delivery),
        ("owner", Delivery::Delivered)
    );
    let say_orch = speak(
        &b,
        p,
        "say",
        json!({"to":"orchestrator","body":"fyi","owner":true}),
    )
    .await
    .unwrap();
    assert_eq!(
        (say_orch.thread.as_str(), say_orch.delivery),
        ("owner", Delivery::Delivered)
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
        json!({"to":"orchestrator","body":"go?","owner":true}),
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
        ("orchestrator", "owner")
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
            (MessageVerb::Say, "orchestrator", Some("work"), None, None),
            (
                MessageVerb::Ask,
                "owner",
                Some("work"),
                Some(QuestionState::Answered),
                Some(reply.id)
            ),
            (
                MessageVerb::Reply,
                "orchestrator",
                Some("owner"),
                None,
                None
            ),
        ]
    );
    assert_eq!(thread[2].to_message, Some(ask_step.id));
    let owner = read_messages(&b, p, json!({"view":"thread","thread":"owner"})).await;
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
    // The orchestrator's: its open question first, then its unread notes and replies.
    assert_eq!(
        inbox(false).await,
        vec![ask_orch.id, say_orch.id, closed.id, after.id]
    );
    assert_eq!(inbox(true).await, vec![reply.id]);
}
#[tokio::test]
async fn a_missing_unknown_or_removed_recipient_is_invalid_and_stores_nothing() {
    let (_home, b, _f, p) = setup().await;
    let before = message_count(&b, p).await;
    for (verb, args) in [
        ("say", json!({"body":"x"})),
        ("say", json!({"to":"","body":"x"})),
        ("ask", json!({"to":"nobody","body":"x"})),
        ("ask", json!({"to":"Work","body":"x"})),
        ("say", json!({"to":"cli","body":"x"})),
        ("say", json!({"to":"orchestrator","body":"to myself"})),
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
#[tokio::test]
async fn receipts_follow_the_step_from_pending_through_its_run_to_done() {
    let (_home, b, f, p) = setup().await;
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
    let own = speak(
        &b,
        p,
        "ask",
        json!({"to":"orchestrator","body":"which?","run":l.identity.run}),
    )
    .await
    .unwrap();
    assert_eq!(
        (own.thread.as_str(), own.delivery),
        ("step-work", Delivery::Delivered)
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
    let home = home::ScratchHome::new().unwrap();
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
    while !socket.exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    b.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&b, p, "s").await.unwrap();
    let launches = f.0.lock().unwrap().clone();
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
    let held = tokio::spawn({
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
    tokio::time::sleep(Duration::from_millis(200)).await;
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
    // An old agent's root post with no `to` asks the orchestrator; a note says it.
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
            (MessageVerb::Ask, "work", Some("orchestrator"), Some(run)),
            (MessageVerb::Say, "work", Some("orchestrator"), Some(run)),
            (MessageVerb::Reply, "orchestrator", Some("work"), None),
            (MessageVerb::Say, "work", Some("owner"), Some(run)),
        ]
    );
    assert_eq!((thread[0].id, thread[1].id), (asked, said));
    assert_eq!(thread[0].state, Some(QuestionState::Answered));
}
#[tokio::test]
async fn a_watcher_sees_exactly_the_pinned_fields_of_each_verb_record() {
    let (_home, b, _f, p) = setup().await;
    let ask = speak(
        &b,
        p,
        "ask",
        json!({"to":"owner","body":"q","title":"T","ui":"root = Button()","data":{"k":1}}),
    )
    .await
    .unwrap();
    speak(&b, p, "say", json!({"to":"work","body":"s"}))
        .await
        .unwrap();
    speak(
        &b,
        p,
        "reply",
        json!({"to_message":ask.id,"body":"","answer":{"action":"close"},"owner":true}),
    )
    .await
    .unwrap();
    let CommandReply::Records(page) = b
        .command(request(json!({"command":"log_read","args":{"project":{"kind":"id","value":p},"since_seq":null,"kinds":["message"],"threads":null,"limit":10}})))
        .await
        .unwrap()
    else {
        panic!("records")
    };
    let keys: Vec<Vec<String>> = page
        .records
        .iter()
        .map(|r| {
            let mut keys: Vec<String> = serde_json::to_value(r)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            keys.sort();
            keys
        })
        .collect();
    let sorted = |names: &[&str]| {
        let mut names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        names.sort();
        names
    };
    let envelope = [
        "seq",
        "at",
        "project",
        "kind",
        "id",
        "verb",
        "from",
        "to",
        "thread",
        "body",
        "posted_at",
    ];
    assert_eq!(
        keys,
        vec![
            sorted(&[&envelope[..], &["title", "ui", "data"]].concat()),
            sorted(&envelope),
            sorted(&[&envelope[..], &["to_message", "answer"]].concat()),
        ]
    );
    let first = serde_json::to_value(&page.records[0]).unwrap();
    assert_eq!(
        (first["kind"].as_str(), first["verb"].as_str()),
        (Some("message"), Some("ask"))
    );
    // The messages rows carry the same shape, plus a question's state.
    let rows = read_messages(&b, p, json!({"view":"history"})).await;
    let row = serde_json::to_value(&rows[0]).unwrap();
    let mut keys: Vec<_> = row.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        sorted(&[
            "id", "verb", "from", "to", "thread", "body", "title", "ui", "data", "at", "state"
        ])
    );
    assert_eq!(row["state"], "closed");
}
#[tokio::test]
async fn current_reserved_run_accepts_submissions_before_claim_and_after_feedback() {
    let (_home, b, f, p) = setup().await;
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
            b.command(request(json!({"command":"step_retry","args":{"project":{"kind":"id","value":p},"selection":{"steps":["work"],"tags":null},"message":"Continue with feedback","reason":"fixture","author":"fixture"}}))).await.unwrap();
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
    b.command(request(json!({"command":"say","args":{"project":{"kind":"id","value":p},"body":"feedback","to":"work","data":null,"run":null}}))).await.unwrap();
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
#[tokio::test]
async fn board_set_and_plan_edits_warn_for_the_steps_the_board_loses() {
    let (_home, b, _f, p) = setup().await;
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

#[tokio::test]
async fn a_running_step_that_has_submitted_is_finishing_in_status_step_context_and_call_status() {
    let (_home, b, f, p) = setup().await;
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
}

/// A coordinator whose plan's `work` runs `agent.run` (codex) in a fresh git repository.
async fn agent_setup() -> (
    home::ScratchHome,
    Coordinator<Fake>,
    Fake,
    ProjectId,
    std::path::PathBuf,
) {
    let home = home::ScratchHome::new().unwrap();
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

#[tokio::test]
async fn step_settle_refuses_a_fn_that_composes_its_agent_with_the_way_by_hand() {
    // fixture.submit stands for a project fn that runs an agent (ctx.builtin("agent.run")):
    // its outputs are what it returns, which sluice cannot derive from the submission.
    let (_home, b, f, p) = setup().await;
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
#[tokio::test]
async fn a_run_completes_from_its_attempt_after_the_plan_moved_on() {
    let (_home, b, f, p) = setup().await;
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
#[tokio::test]
async fn plan_edit_refuses_empty_supplies_and_a_stale_revision_before_preparing() {
    let (_home, b, _f, p) = setup().await;
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
#[tokio::test]
async fn verify_reports_a_hand_corrupted_index_row() {
    let (_home, b, _f, p) = setup().await;
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
