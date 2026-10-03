#[path = "../../../tests/support/home.rs"]
mod home;
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
}
struct Fixture {
    home: ScratchHome,
    broker: Coordinator<Fake>,
    host: Fake,
    project: ProjectId,
}
impl Fixture {
    async fn new(doc: Value, resources: Value) -> Self {
        let home = ScratchHome::new().unwrap();
        assert!(home.root().exists());
        ScratchHome::validate(home.path()).unwrap();
        let host = Fake::default();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), host.clone())
            .await
            .unwrap();
        let reply=command(&broker,json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":resources,"author":"test"}})).await;
        let CommandReply::Project(p) = reply else {
            panic!("project reply")
        };
        let project = p.project_id;
        command(&broker,json!({"command":"plan_patch","args":{"project":json!({"kind":"id","value":project}),"rev":1,"ops":[{"op":"replace","path":"","value":doc}],"start":true,"dry_run":false,"reason":"test","author":"test"}})).await;
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
        let CommandReply::Data(v)=command(&self.broker,json!({"command":"status","args":{"project":json!({"kind":"id","value":self.project}),"selection":{"steps":null,"tags":null}}})).await else{panic!("status")};
        v.into_value()
    }
    async fn finish(&self, launch: &Launch, result: PayloadResult) {
        self.broker.complete(journal(launch, result)).await.unwrap();
    }
    async fn retry(&self) {
        command(&self.broker,json!({"command":"step_retry","args":{"project":json!({"kind":"id","value":self.project}),"selection":{"steps":["each"],"tags":null},"message":"try again","reason":"test","author":"test"}})).await;
    }
    async fn patch(&self, rev: u64, ops: Value) {
        command(&self.broker,json!({"command":"plan_patch","args":{"project":json!({"kind":"id","value":self.project}),"rev":rev,"ops":ops,"start":true,"dry_run":false,"reason":"test","author":"test"}})).await;
    }
}
async fn command(b: &Coordinator<Fake>, v: Value) -> CommandReply {
    b.command(decode_json(&serde_json::to_vec(&v).unwrap()).unwrap())
        .await
        .unwrap()
}
fn map(v: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&v).unwrap()).unwrap()
}
fn journal(l: &Launch, result: PayloadResult) -> CompletionJournal {
    CompletionJournal {
        protocol: 1,
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
#[tokio::test]
async fn readiness_fan_in_outputs_and_inline_quiescence() {
    let f=Fixture::new(json!({"inputs":{},"outputs":{"total":{"source":"c/value"}},"steps":{"a":{"run":"core.echo","in":{"value":{"default":4}}},"b":{"run":"fixture.echo","in":{"value":{"source":"a/value"}}},"c":{"run":"core.echo","in":{"value":{"source":"b/value"}}}}}),json!({})).await;
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
#[tokio::test]
async fn failed_outputs_and_spawn_failure_block_only_their_dependents() {
    let f=Fixture::new(json!({"steps":{"bad":step(json!("spawn-fail")),"other":step(json!(2)),"dependent":{"run":"core.echo","in":{"value":{"source":"bad/value"}}}}}),json!({})).await;
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
#[tokio::test]
async fn scatter_reserves_all_items_holds_once_and_retries_only_failed_item() {
    let f=Fixture::new(json!({"steps":{"each":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2,3]}},"needs":{"cpu":1}},"waiter":{"run":"fixture.echo","in":{"value":{"default":9}},"needs":{"cpu":1}}}}),json!({"cpu":1})).await;
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
#[tokio::test]
async fn changed_scatter_hash_replaces_every_item_and_predecessor() {
    let f=Fixture::new(json!({"steps":{"each":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[1,2]}}}}}),json!({})).await;
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
        json!([{"op":"replace","path":"/steps/each/in/value/default","value":[2,1]}]),
    )
    .await;
    f.tick().await;
    let runs = f.launches();
    assert_eq!(runs.len(), 4);
    assert!(runs[2..].iter().all(|l| l.prev_run.is_none()));
}
#[tokio::test]
async fn priority_ties_and_smaller_requests_that_fit_keep_fair_order() {
    let f=Fixture::new(json!({"steps":{"plain":step(json!(0)),"big":{"run":"fixture.echo","in":{"value":{"default":1}},"needs":{"cpu":3},"priority":100},"first":{"run":"fixture.echo","in":{"value":{"default":2}},"needs":{"cpu":2},"priority":10},"second":{"run":"fixture.echo","in":{"value":{"default":3}},"needs":{"cpu":2},"priority":10},"small":{"run":"fixture.echo","in":{"value":{"default":4}},"needs":{"cpu":1}}}}),json!({"cpu":3})).await;
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
#[tokio::test]
async fn pause_precedes_skip_and_zero_capacity_does_not_block_plain_work() {
    let f=Fixture::new(json!({"inputs":{"go":"boolean"},"steps":{"plain":step(json!(1)),"needs":{"run":"fixture.echo","in":{"value":{"default":2}},"needs":{"cpu":1}},"held":{"run":"fixture.echo","in":{"value":{"default":3}},"after":["go"],"paused":true}}}),json!({"cpu":1})).await;
    command(&f.broker,json!({"command":"project_update","args":{"project":{"kind":"id","value":f.project},"new_name":null,"description":null,"icon":null,"resources":{"cpu":0},"paused":null,"archived":null,"expected_settings_rev":null,"reason":"lower","author":"test"}})).await;
    command(&f.broker,json!({"command":"plan_set_input","args":{"project":json!({"kind":"id","value":f.project}),"name":"go","value":false,"edit":{"expected":2,"dry_run":false,"reason":"test","author":"test"}}})).await;
    f.tick().await;
    assert_eq!(f.launches().len(), 1);
    assert_eq!(f.status().await["steps"]["held"]["status"], "pending");
    f.patch(
        2,
        json!([{"op":"replace","path":"/steps/held/paused","value":false}]),
    )
    .await;
    f.tick().await;
    assert_eq!(f.status().await["steps"]["held"]["status"], "skipped");
    command(&f.broker,json!({"command":"plan_set_input","args":{"project":json!({"kind":"id","value":f.project}),"name":"go","value":true,"edit":{"expected":3,"dry_run":false,"reason":"test","author":"test"}}})).await;
    f.tick().await;
    assert_eq!(f.launches().len(), 2);
}
#[tokio::test]
async fn empty_scatter_and_core_scatter_finish_inline_without_runs() {
    let f=Fixture::new(json!({"steps":{"empty":{"run":"fixture.echo","scatter":"value","in":{"value":{"default":[]}}},"each":{"run":"core.echo","scatter":"value","in":{"value":{"default":[1,2]}}}}}),json!({})).await;
    f.tick().await;
    assert!(f.launches().is_empty());
    let status = f.status().await;
    assert_eq!(status["steps"]["empty"]["outputs"]["value"], json!([]));
    assert_eq!(status["steps"]["each"]["outputs"]["value"], json!([1, 2]));
}
#[tokio::test]
async fn section_waiter_grants_before_plan_admission_even_without_new_ready_work() {
    let f=Fixture::new(json!({"steps":{"active":step(json!(1)),"pending":{"run":"fixture.echo","in":{"value":{"default":2}},"needs":{"cpu":1},"paused":true}}}),json!({"cpu":1})).await;
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
        json!([{"op":"replace","path":"/steps/pending/paused","value":false}]),
    )
    .await;
    f.tick().await;
    assert_eq!(f.launches().len(), 1);
    assert_eq!(f.status().await["resources"]["cpu"]["held"], 1);
}
#[tokio::test]
async fn supervised_capacity_failure_keeps_last_good_observation() {
    let f=Fixture::new(json!({"steps":{"waiting":{"run":"fixture.echo","in":{"value":{"default":1}},"needs":{"cpu":1}}}}),json!({"cpu":{"capacity_fn":"fixture.capacity"}})).await;
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
        json!([{"op":"add","path":"/steps/active/paused","value":true}]),
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

#[tokio::test]
async fn queue_events_are_durable_deduplicated_and_disappear_after_admission() {
    let f = Fixture::new(
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
async fn missing_file_is_typed_failure_and_unrelated_work_progresses() {
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
    f.tick().await;
    let status = f.status().await;
    assert_eq!(status["steps"]["broken"]["status"], "failed");
    assert!(
        status["steps"]["broken"]["error"]
            .to_string()
            .contains("file input")
    );
    assert_eq!(status["steps"]["dependent"]["status"], "pending");
    assert_eq!(status["steps"]["plain"]["outputs"]["value"], "ok");
    assert!(f.launches().is_empty());
}
