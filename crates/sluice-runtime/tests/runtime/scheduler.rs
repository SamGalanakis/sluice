use super::home;
use home::ScratchHome;
use serde_json::{Value, json};
use sluice_model::{
    commands::*,
    error::PublicError,
    ids::*,
    rpc::{FnInvocation, JsonMap, decode_json},
};
use sluice_process::{
    guardian::FnHost,
    journal::{CleanupEvidence, CompletionJournal, PayloadResult},
    socket::CoordinatorCommand,
};
use sluice_runtime::{
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
    scheduler::reconcile_project,
};
use std::sync::{Arc, Mutex};
#[derive(Clone, Default)]
struct Fake {
    launches: Arc<Mutex<Vec<Launch>>>,
    now: Arc<Mutex<Option<std::time::SystemTime>>>,
    /// The host's memory reading; `None` measures nothing.
    memory: Arc<Mutex<Option<sluice_runtime::headroom::HostReading>>>,
}
impl FnHost for Fake {
    async fn invoke(&self, i: FnInvocation) -> Result<JsonMap, PublicError> {
        sluice_runtime::builtins::core::dispatch(
            &i.name,
            &i.inputs,
            &sluice_runtime::builtins::jev::BuiltinCtx::new([]),
        )
        .await
        .map_err(|e| PublicError::FnFailure {
            message: e.to_string(),
        })
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
    fn now(&self) -> std::time::SystemTime {
        self.now
            .lock()
            .unwrap()
            .unwrap_or_else(std::time::SystemTime::now)
    }
    async fn terminal_gone(
        &self,
        _: sluice_model::ids::RunId,
        _: bool,
    ) -> Result<bool, sluice_model::error::PublicError> {
        Ok(true)
    }

    async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, PublicError> {
        let fail = launch
            .invocation
            .inputs
            .0
            .get("value")
            .is_some_and(|v| v.as_value() == "spawn-fail");
        self.launches.lock().unwrap().push(launch);
        Ok(if fail {
            LaunchOutcome::Refused(PublicError::FnFailure {
                message: "spawn refused before process creation".into(),
            })
        } else {
            LaunchOutcome::Accepted
        })
    }
    async fn cleanup_valid(&self, j: &CompletionJournal) -> Result<bool, PublicError> {
        Ok(j.cleanup.iter().all(|p| p.empty))
    }
    async fn host_reading(
        &self,
        _: sluice_model::ids::HomeId,
    ) -> Option<std::io::Result<sluice_runtime::headroom::HostReading>> {
        self.memory.lock().unwrap().clone().map(Ok)
    }
}
struct Fixture {
    home: ScratchHome,
    broker: Coordinator<Fake>,
    host: Fake,
    project: ProjectId,
}
impl Fixture {
    async fn seed(&mut self, doc: Value, resources: Value) {
        self.host.launches.lock().unwrap().clear();
        let reply = command(&self.broker,json!({"command":"project_create","args":{"name":format!("case-{}",ProjectId::new()),"description":"","resources":resources}})).await;
        let CommandReply::Project(p) = reply else {
            panic!("project")
        };
        self.project = p.project_id;
        command(&self.broker,json!({"command":"plan_edit","args":{"project":{"kind":"id","value":self.project},"ops":ops_of(&doc),"reason":"fixture"}})).await;
    }

    async fn new(doc: Value, resources: Value) -> Self {
        let home = ScratchHome::new().unwrap();
        assert!(home.root().exists());
        ScratchHome::validate(home.path()).unwrap();
        std::fs::write(
            home.path().join("config.json"),
            json!({"execution":{
                "defaults":{"memory":"64MiB","cpus":0.1,"tasks":64},
                "guardian_memory":"64MiB"
            }})
            .to_string(),
        )
        .unwrap();
        let host = Fake::default();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), host.clone())
            .await
            .unwrap();
        let reply=command(&broker,json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":resources,"author":"test"}})).await;
        let CommandReply::Project(p) = reply else {
            panic!("project reply")
        };
        let project = p.project_id;
        let ops = ops_of(&doc);
        if !ops.as_array().unwrap().is_empty() {
            command(&broker,json!({"command":"plan_edit","args":{"project":json!({"kind":"id","value":project}),"rev":1,"ops":ops,"start":true,"dry_run":false,"reason":"test","author":"test"}})).await;
        }
        broker.acquire_scheduler("test".into()).await.unwrap();
        Self {
            home,
            broker,
            host,
            project,
        }
    }
    async fn tick(&self) -> usize {
        assert!(self.home.path().exists());
        reconcile_project(&self.broker, self.project, "test")
            .await
            .unwrap()
    }
    fn launches(&self) -> Vec<Launch> {
        self.host.launches.lock().unwrap().clone()
    }
    async fn status(&self) -> Value {
        let CommandReply::Data(v)=command(&self.broker,json!({"command":"status","args":{"project":json!({"kind":"id","value":self.project}),"selection":{"steps":null,"tags":null},"all":true}})).await else{panic!("status")};
        v.into_value()
    }
    async fn finish(&self, launch: &Launch, result: PayloadResult) {
        self.broker.complete(journal(launch, result)).await.unwrap();
    }
    async fn retry(&self) {
        command(&self.broker,json!({"command":"step_retry","args":{"project":json!({"kind":"id","value":self.project}),"selection":{"steps":["each"],"tags":null},"message":"try again","reason":"test","author":"@test"}})).await;
    }
    async fn patch(&self, rev: u64, ops: Value) {
        command(&self.broker,json!({"command":"plan_edit","args":{"project":json!({"kind":"id","value":self.project}),"rev":rev,"ops":ops,"start":true,"dry_run":false,"reason":"test","author":"test"}})).await;
    }
}
/// The `plan_edit` operations that build a plan document from an empty plan: each input,
/// output and step put in the document's order.
fn ops_of(doc: &Value) -> Value {
    let section = |name: &str| doc[name].as_object().cloned().unwrap_or_default();
    let mut ops = vec![];
    for (name, declaration) in section("inputs") {
        ops.push(json!({"op":"input.put","name":name,"declaration":declaration}));
    }
    for (name, binding) in section("outputs") {
        ops.push(json!({"op":"output.put","name":name,"source":binding["source"]}));
    }
    for (step, spec) in section("steps") {
        ops.push(json!({"op":"step.add","step":step,"spec":spec}));
    }
    Value::Array(ops)
}
fn request(v: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&v).unwrap()).unwrap()
}
async fn command(b: &Coordinator<Fake>, v: Value) -> CommandReply {
    b.command(request(v)).await.unwrap()
}
fn map(v: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&v).unwrap()).unwrap()
}
fn journal(l: &Launch, result: PayloadResult) -> CompletionJournal {
    CompletionJournal {
        protocol: 1,
        limits: l.limits,
        guardian_memory_bytes: l.policy.guardian_memory_bytes,
        usage: Some(Default::default()),
        identity: l.identity.clone(),
        completion_id: format!("complete-{}", l.identity.run),
        result,
        starts: vec![],
        exits: vec![],
        cleanup: vec![CleanupEvidence {
            cgroup: format!("fake/{}", l.identity.run),
            empty: true,
            escalated: false,
        }],
        submissions: JsonMap::default(),
        submission_version: None,
        delivery_acks: vec![],
    }
}
fn step(value: Value) -> Value {
    json!({"run":"fixture.echo","in":{"value":{"default":value}}})
}
async fn readiness_fan_in_outputs_and_inline_quiescence(f: &mut Fixture) {
    f.seed(json!({"inputs":{},"outputs":{"total":{"source":"c/value"}},"steps":{"a":{"run":"core.echo","in":{"value":{"default":4}}},"b":{"run":"fixture.echo","in":{"value":{"source":"a/value"}}},"c":{"run":"core.echo","in":{"value":{"source":"b/value"}}}}}),json!({})).await;
    assert_eq!(f.tick().await, 2);
    let launches = f.launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].invocation.inputs.0["value"].as_value(), 4);
    f.finish(
        &launches[0],
        PayloadResult::Succeeded(map(json!({"value":5}))),
    )
    .await;
    assert_eq!(f.tick().await, 1);
    let status = f.status().await;
    assert_eq!(status["outputs"]["total"], 5);
    assert_eq!(status["steps"]["a"]["manual"], 0);
    assert_eq!(status["steps"]["c"]["status"], "succeeded");
    assert_eq!(f.tick().await, 0);
}
async fn failed_outputs_and_spawn_failure_block_only_their_dependents(f: &mut Fixture) {
    f.seed(json!({"steps":{"bad":step(json!("spawn-fail")),"other":step(json!(2)),"dependent":{"run":"core.echo","in":{"value":{"source":"bad/value"}}}}}),json!({})).await;
    f.tick().await;
    let status = f.status().await;
    assert_eq!(status["steps"]["bad"]["status"], "failed");
    assert_eq!(status["steps"]["other"]["status"], "running");
    assert_eq!(status["steps"]["dependent"]["status"], "pending");
    f.finish(
        &f.launches()[1],
        PayloadResult::Succeeded(map(json!({"undeclared":true}))),
    )
    .await;
    assert_eq!(f.status().await["steps"]["other"]["status"], "failed");
}
async fn scatter_with_available_capacity_holds_once_and_retries_only_failed_item(f: &mut Fixture) {
    f.seed(json!({"steps":{"each":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2,3]}},"needs":{"cpu":1}},"waiter":{"run":"fixture.echo","in":{"value":{"default":9}},"needs":{"cpu":1}}}}),json!({"cpu":1})).await;
    assert_eq!(f.tick().await, 3);
    let batch = f.launches();
    assert_eq!(f.status().await["resources"]["cpu"]["held"], 1);
    f.finish(
        &batch[1],
        PayloadResult::Failed(PublicError::FnFailure {
            message: "item failed".into(),
        }),
    )
    .await;
    assert_eq!(f.status().await["steps"]["each"]["status"], "running");
    assert_eq!(f.status().await["resources"]["cpu"]["held"], 1);
    for index in [2, 0] {
        f.finish(
            &batch[index],
            PayloadResult::Succeeded(map(json!({"value":index+1}))),
        )
        .await;
    }
    f.retry().await;
    f.tick().await;
    let retry = f.launches()[3].clone();
    assert_eq!(retry.identity.step.as_ref().unwrap().as_str(), "each");
    assert_eq!(retry.prev_run, Some(batch[1].identity.run));
    assert_eq!(f.launches().len(), 4);
    f.finish(&retry, PayloadResult::Succeeded(map(json!({"value":2}))))
        .await;
    f.tick().await;
    assert_eq!(
        f.status().await["steps"]["each"]["outputs"]["value"],
        json!([1, 2, 3])
    );
    assert_eq!(f.launches().len(), 5);
}
async fn changed_scatter_hash_replaces_every_item_and_predecessor(f: &mut Fixture) {
    f.seed(json!({"steps":{"each":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2]}}}}}),json!({})).await;
    f.tick().await;
    let old = f.launches();
    f.finish(&old[0], PayloadResult::Succeeded(map(json!({"value":1}))))
        .await;
    f.finish(
        &old[1],
        PayloadResult::Failed(PublicError::FnFailure {
            message: "failed".into(),
        }),
    )
    .await;
    f.retry().await;
    f.patch(
        2,
        json!([{"op":"step.update","step":"each","changes":{"in":{"value":{"default":[2,1]}}}}]),
    )
    .await;
    f.tick().await;
    let runs = f.launches();
    assert_eq!(runs.len(), 4);
    assert!(runs[2..].iter().all(|l| l.prev_run.is_none()));
}
async fn priority_ties_and_smaller_requests_that_fit_keep_fair_order(f: &mut Fixture) {
    f.seed(json!({"steps":{"plain":step(json!(0)),"big":{"run":"fixture.echo","in":{"value":{"default":1}},"needs":{"cpu":3},"priority":100},"first":{"run":"fixture.echo","in":{"value":{"default":2}},"needs":{"cpu":2},"priority":10},"second":{"run":"fixture.echo","in":{"value":{"default":3}},"needs":{"cpu":2},"priority":10},"small":{"run":"fixture.echo","in":{"value":{"default":4}},"needs":{"cpu":1}}}}),json!({"cpu":3})).await;
    f.tick().await;
    assert_eq!(
        f.launches()
            .iter()
            .map(|l| l.identity.step.as_ref().unwrap().as_str())
            .collect::<Vec<_>>(),
        ["plain", "big"]
    );
    f.finish(
        &f.launches()[1],
        PayloadResult::Succeeded(map(json!({"value":1}))),
    )
    .await;
    f.tick().await;
    assert_eq!(
        f.launches()[2..]
            .iter()
            .map(|l| l.identity.step.as_ref().unwrap().as_str())
            .collect::<Vec<_>>(),
        ["first", "small"]
    );
}
async fn pause_precedes_skip_and_zero_capacity_does_not_block_plain_work(f: &mut Fixture) {
    f.seed(json!({"inputs":{"go":"boolean"},"steps":{"plain":step(json!(1)),"needs":{"run":"fixture.echo","in":{"value":{"default":2}},"needs":{"cpu":1}},"held":{"run":"fixture.echo","in":{"value":{"default":3}},"after":["go"],"paused":true}}}),json!({"cpu":1})).await;
    command(&f.broker,json!({"command":"project_update","args":{"project":{"kind":"id","value":f.project},"new_name":null,"description":null,"icon":null,"resources":{"cpu":0},"paused":null,"archived":null,"expected_settings_rev":null,"reason":"lower","author":"test"}})).await;
    command(&f.broker,json!({"command":"plan_set_input","args":{"project":json!({"kind":"id","value":f.project}),"name":"go","value":false,"edit":{"expected":2,"dry_run":false,"reason":"test","author":"test"}}})).await;
    f.tick().await;
    assert_eq!(f.launches().len(), 1);
    assert_eq!(f.status().await["steps"]["held"]["status"], "pending");
    f.patch(
        2,
        json!([{"op":"step.update","step":"held","changes":{"paused":false}}]),
    )
    .await;
    f.tick().await;
    assert_eq!(f.status().await["steps"]["held"]["status"], "skipped");
    command(&f.broker,json!({"command":"plan_set_input","args":{"project":json!({"kind":"id","value":f.project}),"name":"go","value":true,"edit":{"expected":3,"dry_run":false,"reason":"test","author":"test"}}})).await;
    f.tick().await;
    assert_eq!(f.launches().len(), 2);
}
async fn empty_scatter_and_core_scatter_finish_inline_without_runs(f: &mut Fixture) {
    f.seed(json!({"steps":{"empty":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[]}}},"each":{"run":"core.echo","scatter":"value","in":{"value":{"default":[1,2]}}}}}),json!({})).await;
    f.tick().await;
    assert!(f.launches().is_empty());
    let status = f.status().await;
    assert_eq!(status["steps"]["empty"]["outputs"]["value"], json!([]));
    assert_eq!(status["steps"]["each"]["outputs"]["value"], json!([1, 2]));
}
async fn section_waiter_grants_before_plan_admission_even_without_new_ready_work(f: &mut Fixture) {
    f.seed(json!({"steps":{"active":step(json!(1)),"pending":{"run":"fixture.echo","in":{"value":{"default":2}},"needs":{"cpu":1},"paused":true}}}),json!({"cpu":1})).await;
    f.tick().await;
    let run = f.launches()[0].identity.run;
    let launch = f.launches()[0].clone();
    let g = sluice_process::socket::GuardianIdentity {
        identity: launch.identity.clone(),
        process: sluice_process::identity::ProcessIdentity {
            pid: 123,
            start_time: 42,
            boot_id: "fake".into(),
            cgroup: "/fake/control".into(),
        },
        unit: format!("sluice-test-{run}.service"),
        socket_challenge: "fake".into(),
    };
    f.broker
        .guardian(CoordinatorCommand::Claim(g), Some(&launch.capability))
        .await
        .unwrap();
    f.broker
        .guardian(
            CoordinatorCommand::Started {
                identity: launch.identity.clone(),
                invocation: launch.invocation.invocation,
                executor: sluice_process::identity::ProcessIdentity {
                    pid: 124,
                    start_time: 43,
                    boot_id: "fake".into(),
                    cgroup: "/fake/payload".into(),
                },
            },
            Some(&launch.capability),
        )
        .await
        .unwrap();
    command(&f.broker,json!({"command":"acquire_lease","args":{"run":run,"resource":"cpu","amount":1,"priority":0,"request_id":"section"}})).await;
    f.patch(
        2,
        json!([{"op":"step.update","step":"pending","changes":{"paused":false}}]),
    )
    .await;
    f.tick().await;
    assert_eq!(f.launches().len(), 1);
    assert_eq!(f.status().await["resources"]["cpu"]["held"], 1);
}
async fn supervised_capacity_failure_keeps_last_good_observation(f: &mut Fixture) {
    f.seed(json!({"steps":{"waiting":{"run":"fixture.echo","in":{"value":{"default":1}},"needs":{"cpu":1}}}}),json!({"cpu":{"capacity_fn":"fixture.capacity"}})).await;
    f.tick().await;
    assert!(f.launches().is_empty());
    f.broker
        .calls()
        .capacity_call(f.project, "cpu".into())
        .await
        .unwrap();
    let call = f.launches()[0].clone();
    f.finish(&call, PayloadResult::Succeeded(map(json!({"capacity":2}))))
        .await;
    f.tick().await;
    assert_eq!(f.launches().len(), 2);
    f.broker
        .calls()
        .capacity_call(f.project, "cpu".into())
        .await
        .unwrap();
    let call = f.launches()[2].clone();
    f.finish(
        &call,
        PayloadResult::Failed(PublicError::FnFailure {
            message: "capacity offline".into(),
        }),
    )
    .await;
    let status = f.status().await;
    assert_eq!(status["resources"]["cpu"]["capacity"], 2);
    assert!(status["resources"]["cpu"]["error"].is_object());
}
#[tokio::test]
async fn pause_running_does_not_cancel_and_lease_conflict_fences_admission() {
    let f = Fixture::new(json!({"steps":{"active":step(json!(1))}}), json!({})).await;
    f.tick().await;
    f.patch(
        2,
        json!([{"op":"step.update","step":"active","changes":{"paused":true}}]),
    )
    .await;
    let launch = f.launches()[0].clone();
    let reply = f
        .broker
        .guardian(
            CoordinatorCommand::CancelIntent(launch.identity.clone()),
            Some(&launch.capability),
        )
        .await
        .unwrap();
    assert!(matches!(
        reply,
        sluice_process::socket::CoordinatorReply::CancelIntent(false)
    ));
    assert!(f.broker.acquire_scheduler("second".into()).await.is_err());
    f.broker.release_scheduler("test".into()).await.unwrap();
    assert!(
        reconcile_project(&f.broker, f.project, "test")
            .await
            .is_err()
    );
    f.finish(&launch, PayloadResult::Succeeded(map(json!({"value":1}))))
        .await;
    assert_eq!(f.status().await["steps"]["active"]["status"], "succeeded");
}

async fn queue_events_are_durable_deduplicated_and_disappear_after_admission(f: &mut Fixture) {
    f.seed(
        json!({"steps":{
            "a":{"run":"fixture.echo","in":{"value":{"default":1}},"needs":{"cpu":1}},
            "b":{"run":"fixture.echo","in":{"value":{"default":2}},"needs":{"cpu":1}},
            "inline":{"run":"core.echo","in":{"value":{"default":3}},"needs":{"cpu":1}}
        }}),
        json!({"cpu":1}),
    )
    .await;
    for _ in 0..5 {
        f.tick().await;
    }
    assert_eq!(f.launches().len(), 1);
    let status = f.status().await;
    assert_eq!(status["steps"]["b"]["queued"], json!(["cpu"]));
    assert_eq!(status["steps"]["inline"]["status"], "pending");
    assert_eq!(
        status["steps"]["inline"]["waiting"],
        json!(["queued: needs cpu 1 (1/1 held)"])
    );
    let project = f.project;
    let queued = f
        .broker
        .reads()
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT count(*) FROM records WHERE project_id=?1 AND kind='step.queued'",
                [project.to_string()],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(queued, 2);
    f.finish(
        &f.launches()[0],
        PayloadResult::Succeeded(map(json!({"value":1}))),
    )
    .await;
    f.tick().await;
    assert!(f.status().await["steps"]["b"].get("queued").is_none());
    f.finish(
        &f.launches()[1],
        PayloadResult::Succeeded(map(json!({"value":2}))),
    )
    .await;
    f.tick().await;
    assert_eq!(f.status().await["steps"]["inline"]["outputs"]["value"], 3);
    assert!(f.status().await["steps"]["inline"].get("queued").is_none());
}

#[tokio::test]
async fn status_includes_never_run_steps_and_duplicate_names_their_state() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing.txt");
    let f = Fixture::new(
        json!({"steps":{
            "broken":{"run":"fixture.echo","in":{"value":{"file":missing}}},
            "dependent":{"run":"core.echo","in":{"value":{"source":"broken/value"}}},
            "plain":{"run":"core.echo","in":{"value":{"default":"ok"}}}
        }}),
        json!({}),
    )
    .await;
    let status_request = json!({"command":"status","args":{
        "project":{"kind":"id","value":f.project},"selection":{"steps":null,"tags":null}
    }});
    let CommandReply::Data(pending) = command(&f.broker, status_request.clone()).await else {
        panic!("status")
    };
    assert_eq!(pending.as_value()["steps"]["broken"]["status"], "pending");
    assert_eq!(pending.as_value()["steps"]["broken"]["run_ids"], json!([]));
    let duplicate = f
        .broker
        .command(request(json!({
            "command":"step_add","args":{
                "project":{"kind":"id","value":f.project},"step":"broken",
                "spec":{"run":"core.external"},"edit":{"reason":"duplicate"}
            }
        })))
        .await
        .unwrap_err();
    assert!(
        serde_json::to_string(&duplicate)
            .unwrap()
            .contains("step broken already exists (status: pending)"),
        "{duplicate:?}"
    );
    f.tick().await;
    let CommandReply::Data(visible) = command(&f.broker, status_request).await else {
        panic!("status")
    };
    assert_eq!(visible.as_value()["steps"]["broken"]["status"], "failed");
    let status = f.status().await;
    assert_eq!(status["steps"].as_object().unwrap().len(), 3);
    assert_eq!(status["steps"]["broken"]["status"], "failed");
    assert!(
        status["steps"]["broken"]["error"]
            .to_string()
            .contains("file input")
    );
    assert_eq!(status["steps"]["dependent"]["status"], "pending");
    assert_eq!(status["steps"]["plain"]["outputs"]["value"], "ok");
    assert!(f.launches().is_empty());
    assert_eq!(status["steps"]["broken"]["run_ids"], json!([]));
    let duplicate = f
        .broker
        .command(request(json!({
            "command":"plan_edit","args":{
                "project":{"kind":"id","value":f.project},"reason":"duplicate",
                "ops":[{"op":"step.add","step":"broken","spec":{"run":"core.external"}}]
            }
        })))
        .await
        .unwrap_err();
    assert!(
        serde_json::to_string(&duplicate)
            .unwrap()
            .contains("step broken already exists (status: failed)"),
        "{duplicate:?}"
    );
}
#[tokio::test]
async fn a_guardian_watch_is_held_until_its_message_or_cancel_and_repeated_reports_do_not_write() {
    use sluice_process::socket::CoordinatorReply;
    let f = Fixture::new(json!({"steps":{"active":step(json!(1))}}), json!({})).await;
    f.tick().await;
    let launch = f.launches()[0].clone();
    let run = launch.identity.run;
    let cap = Some(&launch.capability);
    let process = |pid, cgroup: &str| sluice_process::identity::ProcessIdentity {
        pid,
        start_time: 42,
        boot_id: "fake".into(),
        cgroup: cgroup.into(),
    };
    let g = sluice_process::socket::GuardianIdentity {
        identity: launch.identity.clone(),
        process: process(123, "/fake/control"),
        unit: format!("sluice-test-{run}.service"),
        socket_challenge: "fake".into(),
    };
    f.broker
        .guardian(CoordinatorCommand::Claim(g), cap)
        .await
        .unwrap();
    let started = || CoordinatorCommand::Started {
        identity: launch.identity.clone(),
        invocation: launch.invocation.invocation,
        executor: process(124, "/fake/payload"),
    };
    f.broker.guardian(started(), cap).await.unwrap();
    // What an older guardian repeats every 50 ms: none of it writes.
    let writes = f.broker.writer().transactions();
    for _ in 0..20 {
        f.broker.guardian(started(), cap).await.unwrap();
        f.broker
            .guardian(
                CoordinatorCommand::CancelIntent(launch.identity.clone()),
                cap,
            )
            .await
            .unwrap();
        f.broker
            .guardian(
                CoordinatorCommand::Messages {
                    identity: launch.identity.clone(),
                    after: MessageId(0),
                    through: None,
                    limit: 128,
                },
                cap,
            )
            .await
            .unwrap();
    }
    assert_eq!(f.broker.writer().transactions(), writes);
    let watch = |after| {
        f.broker.guardian(
            CoordinatorCommand::Watch {
                identity: launch.identity.clone(),
                after,
                wait_ms: 30_000,
            },
            cap,
        )
    };
    let held = watch(MessageId(0));
    tokio::pin!(held);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut held)
            .await
            .is_err(),
        "nothing to report yet"
    );
    // Commits elsewhere neither wake nor cost the held watch a read.
    let reads = f.broker.reads().snapshots();
    for _ in 0..20 {
        tokio::select! {
            biased;
            _ = &mut held => panic!("woke for an unrelated commit"),
            written = f.broker.writer().write(sluice_store::RetrySafety::NonIdempotent, |tx| {
                tx.changed(None, "unrelated");
                Ok(())
            }) => written.unwrap(),
        }
    }
    assert_eq!(f.broker.reads().snapshots(), reads);
    let project = json!({"kind":"id","value":f.project});
    let CommandReply::Receipt(MessageReceipt { id, .. }) = command(&f.broker, json!({"command":"say","args":{"project":project,"body":"hello","owner":true,"to":"active"}})).await else {
        panic!("post")
    };
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), held)
        .await
        .unwrap()
        .unwrap();
    let CoordinatorReply::Watched {
        cancelled,
        messages,
    } = reply
    else {
        panic!("watched")
    };
    assert!(!cancelled);
    assert_eq!(messages.iter().map(|m| m.id).collect::<Vec<_>>(), vec![id]);
    // The next watch holds again until the step is cancelled.
    let held = watch(id);
    tokio::pin!(held);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut held)
            .await
            .is_err(),
        "nothing new yet"
    );
    command(&f.broker, json!({"command":"step_cancel","args":{"project":project,"selection":{"steps":["active"],"tags":null},"reason":"stop","author":"test"}})).await;
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), held)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        reply,
        CoordinatorReply::Watched {
            cancelled: true,
            ..
        }
    ));
}

#[tokio::test]
async fn readiness_inline_and_independent_failures() {
    let mut f = Fixture::new(json!({"steps":{}}), json!({})).await;
    readiness_fan_in_outputs_and_inline_quiescence(&mut f).await;
    failed_outputs_and_spawn_failure_block_only_their_dependents(&mut f).await;
    empty_scatter_and_core_scatter_finish_inline_without_runs(&mut f).await;
}

#[tokio::test]
async fn scatter_retry_and_changed_inputs() {
    let mut f = Fixture::new(json!({"steps":{}}), json!({})).await;
    scatter_with_available_capacity_holds_once_and_retries_only_failed_item(&mut f).await;
    changed_scatter_hash_replaces_every_item_and_predecessor(&mut f).await;
}

#[tokio::test]
async fn capacity_ordering_pauses_and_queue_records() {
    let mut f = Fixture::new(json!({"steps":{}}), json!({})).await;
    priority_ties_and_smaller_requests_that_fit_keep_fair_order(&mut f).await;
    pause_precedes_skip_and_zero_capacity_does_not_block_plain_work(&mut f).await;
    section_waiter_grants_before_plan_admission_even_without_new_ready_work(&mut f).await;
    supervised_capacity_failure_keeps_last_good_observation(&mut f).await;
    queue_events_are_durable_deduplicated_and_disappear_after_admission(&mut f).await;
}

fn bounded_policy(ceiling: &str) -> Value {
    json!({"execution":{
        "defaults":{"memory":"16MiB","cpus":0.2,"tasks":8},
        "ceilings":{"memory":ceiling,"cpus":0.2,"tasks":8},
        "total":{"memory":"128MiB","cpus":1,"tasks":128},
        "guardian_memory":"16MiB"
    }})
}

#[tokio::test]
async fn bounded_scatter_keeps_original_inputs_gates_and_files_between_admissions() {
    let mut f = Fixture::new(json!({"steps":{}}), json!({})).await;
    let brief = f.home.root().join("brief.txt");
    std::fs::write(&brief, "original brief").unwrap();
    // The host has room for one run's admission charge at a time.
    let mut policy = bounded_policy("96MiB");
    policy["execution"]["host_reserve"] = json!("1GiB");
    policy["execution"]["admission_charge"] = json!("64MiB");
    std::fs::write(f.home.path().join("config.json"), policy.to_string()).unwrap();
    *f.host.memory.lock().unwrap() = Some(sluice_runtime::headroom::HostReading {
        available_bytes: (1 << 30) + (64 << 20),
        ..Default::default()
    });
    f.seed(
        json!({"inputs":{"items":"int[]","go":"boolean"},"steps":{
            "each":{"run":"fixture.submit","scatter":"value","after":["go"],"needs":{"lane":1},
            "in":{"value":{"source":"items"},"brief":{"file":brief}}}
        }}),
        json!({"lane":1}),
    )
    .await;
    for (name, value) in [("items", json!([1, 2, 3])), ("go", json!(true))] {
        command(&f.broker,json!({"command":"plan_set_input","args":{"project":{"kind":"id","value":f.project},"name":name,"value":value,"edit":{"expected":2,"dry_run":false,"reason":"initial","author":"test"}}})).await;
    }
    assert_eq!(f.tick().await, 1);
    let first = f.launches()[0].clone();
    assert_eq!(first.invocation.inputs.0["value"].as_value(), 1);
    assert_eq!(
        first.invocation.inputs.0["brief"].as_value(),
        "original brief"
    );
    for (name, value) in [("items", json!([9])), ("go", json!(false))] {
        command(&f.broker,json!({"command":"plan_set_input","args":{"project":{"kind":"id","value":f.project},"name":name,"value":value,"edit":{"expected":2,"dry_run":false,"reason":"next batch","author":"test"}}})).await;
    }
    std::fs::remove_file(&brief).unwrap();
    assert_eq!(f.tick().await, 0);
    f.finish(&first, PayloadResult::Succeeded(map(json!({"value":1}))))
        .await;
    f.patch(
        2,
        json!([{"op":"step.update","step":"each","changes":{"paused":true}}]),
    )
    .await;
    assert_eq!(f.tick().await, 0);
    f.patch(
        3,
        json!([{"op":"step.update","step":"each","changes":{"paused":false}}]),
    )
    .await;
    assert_eq!(f.tick().await, 1);
    let second = f.launches()[1].clone();
    assert_eq!(second.invocation.inputs.0["value"].as_value(), 2);
    assert_eq!(
        second.invocation.inputs.0["brief"].as_value(),
        "original brief"
    );
    assert_eq!(f.status().await["resources"]["lane"]["held"], 1);
    f.finish(&second, PayloadResult::Succeeded(map(json!({"value":2}))))
        .await;
    assert_eq!(f.tick().await, 1);
    let third = f.launches()[2].clone();
    assert_eq!(third.invocation.inputs.0["value"].as_value(), 3);
    assert_eq!(
        third.invocation.inputs.0["brief"].as_value(),
        "original brief"
    );
    f.finish(&third, PayloadResult::Succeeded(map(json!({"value":3}))))
        .await;
    assert_eq!(
        f.status().await["steps"]["each"]["outputs"]["value"],
        json!([1, 2, 3])
    );
    assert_eq!(f.status().await["resources"]["lane"]["held"], 0);
}

#[tokio::test]
async fn lowering_a_run_ceiling_queues_that_step_without_blocking_other_work() {
    let mut f = Fixture::new(json!({"steps":{}}), json!({})).await;
    std::fs::write(
        f.home.path().join("config.json"),
        bounded_policy("64MiB").to_string(),
    )
    .unwrap();
    f.seed(
        json!({"steps":{
            "a":{"run":"fixture.echo","limits":{"memory":"48MiB"},"in":{"value":{"default":1}}},
            "b":{"run":"fixture.echo","in":{"value":{"default":2}}}
        }}),
        json!({}),
    )
    .await;
    std::fs::write(
        f.home.path().join("config.json"),
        bounded_policy("32MiB").to_string(),
    )
    .unwrap();
    assert_eq!(f.tick().await, 1);
    assert_eq!(
        f.launches()[0].identity.step.as_ref().unwrap().as_str(),
        "b"
    );
    let status = f.status().await;
    assert_eq!(status["steps"]["a"]["status"], "pending");
    assert!(
        status["steps"]["a"]["waiting"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .is_some_and(|reason| reason.contains("above the per-run memory ceiling"))),
        "{status}"
    );
}

// Regression: terminated engine lanes and overseers used to need a polling cron invoking
// step_retry. Drive coordinator completion and admission with the host clock; no sleeps.
#[tokio::test]
async fn restart_policies_wait_resume_limit_and_stop_on_cancel() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let spec = |restart: &str| json!({"run":"fixture.echo","restart":restart,"in":{"value":{"default":7}}});
    let mut f = Fixture::new(
        json!({"steps":{
            "engine":spec("on_engine_error"),
            "quota":spec("on_engine_error"),
            "legacy":spec("on_engine_error"),
            "output":spec("on_engine_error"),
            "mixed":{"run":"fixture.echo","restart":"on_engine_error","scatter":"value","in":{"value":{"default":[1,2]}}},
            "work-failure":spec("on_engine_error"),
            "success":spec("on_engine_error"),
            "deadline":spec("on_engine_error"),
            "pipe":spec("on_engine_error"),
            "cancelled":spec("always"),
            "watch":spec("always")
        }}),
        json!({}),
    )
    .await;
    f.tick().await;
    let launches = f.launches();
    let find = |name: &str| {
        launches
            .iter()
            .find(|l| l.identity.step.as_ref().unwrap().as_str() == name)
            .unwrap()
    };
    let reset = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 7200;
    let failure = |kind: &str, retry_at| {
        PayloadResult::Failed(PublicError::AgentFailure {
            kind: kind.into(),
            message: format!("{kind}: engine stopped"),
            session: Some("session-1".into()),
            retry_at,
        })
    };
    for launch in launches
        .iter()
        .filter(|l| l.identity.step.as_ref().unwrap().as_str() == "mixed")
    {
        let result = if launch.invocation.inputs.0["value"].as_value() == 1 {
            failure("EngineExited", None)
        } else {
            PayloadResult::Succeeded(map(json!({"undeclared":true})))
        };
        f.finish(launch, result).await;
    }
    f.finish(find("engine"), failure("UnknownAcceptance", None))
        .await;
    f.finish(find("deadline"), failure("OperationTimeout", None))
        .await;
    f.finish(
        find("pipe"),
        PayloadResult::Lost("guardian broken pipe".into()),
    )
    .await;
    f.finish(
        find("work-failure"),
        PayloadResult::Failed(PublicError::FnFailure {
            message: "work output failed".into(),
        }),
    )
    .await;
    f.finish(
        find("success"),
        PayloadResult::Succeeded(map(json!({"value":7}))),
    )
    .await;
    f.finish(find("legacy"), PayloadResult::Failed(PublicError::AgentFailure {
        kind:"QuotaExhausted".into(), message:"claude: weekly limit reached; resets 2099-01-01T00:00Z (in many days) — wait for reset".into(), session:Some("legacy-session".into()), retry_at:None,
    })).await;
    f.finish(find("quota"), failure("QuotaExhausted", Some(reset)))
        .await;
    f.finish(
        find("output"),
        PayloadResult::Succeeded(map(json!({"undeclared":true}))),
    )
    .await;
    f.finish(
        find("cancelled"),
        PayloadResult::Cancelled("owner stopped it".into()),
    )
    .await;
    f.finish(
        find("watch"),
        PayloadResult::Succeeded(map(json!({"value":7}))),
    )
    .await;
    assert_eq!(f.tick().await, 0, "backoff before every relaunch");
    *f.host.now.lock().unwrap() = Some(SystemTime::now() + Duration::from_secs(11));
    assert_eq!(f.tick().await, 4);
    let retries = f.launches();
    for name in ["engine", "watch", "deadline", "pipe"] {
        let retry = retries
            .iter()
            .rev()
            .find(|l| l.identity.step.as_ref().unwrap().as_str() == name)
            .unwrap();
        assert_eq!(
            retry.prev_run,
            Some(find(name).identity.run),
            "same-session predecessor"
        );
        assert!(
            retry.assigned.through > retry.assigned.after,
            "restart reason delivered as feedback"
        );
    }
    let status = f.status().await;
    assert_eq!(status["steps"]["engine"]["restart_count"], 1);
    assert_eq!(status["steps"]["quota"]["restart_count"], 0);
    assert_eq!(status["steps"]["quota"]["restart_at"], reset);
    assert!(
        status["steps"]["quota"]["error"]["retry_at"].is_null(),
        "pinned binaries retain the old error shape"
    );
    assert_eq!(status["steps"]["legacy"]["restart_at"], 4070908862_i64);
    assert_eq!(status["steps"]["output"]["status"], "failed");
    assert_eq!(status["steps"]["mixed"]["restart_count"], 0);
    assert_eq!(status["steps"]["cancelled"]["status"], "failed");
    assert_eq!(status["steps"]["work-failure"]["restart_count"], 0);
    assert_eq!(status["steps"]["success"]["restart_count"], 0);
    let CommandReply::Data(units) = command(&f.broker,json!({"command":"status","args":{"project":{"kind":"id","value":f.project},"view":"units","all":true,"selection":{}}})).await else {panic!("units")};
    assert_eq!(
        units.into_value()["units"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["unit"] == "watch")
            .unwrap()["restart"]["watch"]["count"],
        1
    );
    let records = command(&f.broker,json!({"command":"log_read","args":{"project":{"kind":"id","value":f.project},"kinds":["message"],"limit":100}})).await;
    assert!(
        serde_json::to_string(&records)
            .unwrap()
            .contains("UnknownAcceptance: engine stopped")
    );
    command(&f.broker,json!({"command":"step_cancel","args":{"project":{"kind":"id","value":f.project},"selection":{"steps":["mixed"]},"reason":"stop mixed"}})).await;
    command(&f.broker,json!({"command":"step_update","args":{"project":{"kind":"id","value":f.project},"step":"mixed","changes":{"restart":"always"},"edit":{}}})).await;
    // The policy can change while a run is live, through the public command.
    command(&f.broker,json!({"command":"step_update","args":{"project":{"kind":"id","value":f.project},"step":"engine","changes":{"restart":"always"},"edit":{"reason":"watch","author":"test"}}})).await;
    assert_eq!(f.status().await["steps"]["engine"]["restart"], "always");
    *f.host.now.lock().unwrap() = Some(UNIX_EPOCH + Duration::from_secs(reset));
    assert_eq!(
        f.tick().await,
        1,
        "quota waits through its reset: {:?}",
        f.launches()
            .iter()
            .map(|l| l.identity.step.as_ref().unwrap().as_str())
            .collect::<Vec<_>>()
    );
    let all = f.launches();
    let quota = all.last().unwrap();
    assert_eq!(quota.prev_run, Some(find("quota").identity.run));
    // One overseer repeatedly succeeds. The seventh automatic restart waits for the
    // oldest of six attempts to leave the rolling hour, even after repeated ticks.
    let mut watch = retries
        .iter()
        .rev()
        .find(|l| l.identity.step.as_ref().unwrap().as_str() == "watch")
        .unwrap()
        .clone();
    let mut now = reset;
    for count in 2..=6 {
        f.finish(&watch, PayloadResult::Succeeded(map(json!({"value":7}))))
            .await;
        now += 301;
        *f.host.now.lock().unwrap() = Some(UNIX_EPOCH + Duration::from_secs(now));
        assert_eq!(f.tick().await, 1);
        watch = f.launches().last().unwrap().clone();
        assert_eq!(f.status().await["steps"]["watch"]["restart_count"], count);
    }
    // This first retry was two hours ago; add one more within this hour.
    f.finish(
        &watch,
        PayloadResult::Failed(PublicError::FnFailure {
            message: "overseer failed".into(),
        }),
    )
    .await;
    now += 301;
    *f.host.now.lock().unwrap() = Some(UNIX_EPOCH + Duration::from_secs(now));
    assert_eq!(f.tick().await, 1, "always also restarts ordinary failures");
    watch = f.launches().last().unwrap().clone();
    f.finish(&watch, PayloadResult::Succeeded(map(json!({"value":7}))))
        .await;
    now += 301;
    *f.host.now.lock().unwrap() = Some(UNIX_EPOCH + Duration::from_secs(now));
    // Reopening the coordinator must retain the budget and counts.
    f.broker.writer().shutdown().await.unwrap();
    f.broker = Coordinator::open(f.home.path().into(), Catalog::fixtures(), f.host.clone())
        .await
        .unwrap();
    f.broker.acquire_scheduler("test".into()).await.unwrap();
    assert_eq!(
        f.tick().await,
        0,
        "six per rolling hour survives coordinator replacement"
    );
    assert_eq!(f.status().await["steps"]["watch"]["restart_count"], 7);
    // Cancelling during the backoff after success must stop an overseer too.
    command(&f.broker,json!({"command":"step_cancel","args":{"project":{"kind":"id","value":f.project},"selection":{"steps":["watch"]},"reason":"stop overseer"}})).await;
    *f.host.now.lock().unwrap() = Some(UNIX_EPOCH + Duration::from_secs(now + 7200));
    assert_eq!(f.tick().await, 0);
}

// A policy is local to its step. The manual retry walk also re-arms failed dependents;
// automatically doing that would silently retry somebody else's output failure/cancel.
#[tokio::test]
async fn automatic_restart_does_not_rearm_failed_or_cancelled_dependents() {
    use std::time::{Duration, SystemTime};
    let f = Fixture::new(
        json!({"steps":{
            "parent":{"run":"fixture.echo","in":{"value":{"default":7}}},
            "child":{"run":"fixture.echo","in":{"value":{"source":"parent/value"}}}
        }}),
        json!({}),
    )
    .await;
    f.tick().await;
    f.finish(
        &f.launches()[0],
        PayloadResult::Succeeded(map(json!({"value":7}))),
    )
    .await;
    f.tick().await;
    f.finish(
        &f.launches()[1],
        PayloadResult::Succeeded(map(json!({"undeclared":true}))),
    )
    .await;
    command(&f.broker,json!({"command":"step_update","args":{"project":{"kind":"id","value":f.project},"step":"parent","changes":{"restart":"always"},"edit":{}}})).await;
    *f.host.now.lock().unwrap() = Some(SystemTime::now() + Duration::from_secs(11));
    assert_eq!(f.tick().await, 1);
    assert_eq!(f.status().await["steps"]["child"]["status"], "failed");
    command(&f.broker,json!({"command":"step_cancel","args":{"project":{"kind":"id","value":f.project},"selection":{"steps":["child"]},"reason":"leave stopped"}})).await;
    f.finish(
        &f.launches()[2],
        PayloadResult::Succeeded(map(json!({"value":7}))),
    )
    .await;
    *f.host.now.lock().unwrap() = Some(SystemTime::now() + Duration::from_secs(40));
    assert_eq!(f.tick().await, 1);
    assert_eq!(
        f.status().await["steps"]["child"]["error"]["error"],
        "cancelled"
    );
}
