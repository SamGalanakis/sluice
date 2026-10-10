//! The contention gate of plan-rows §11, through the coordinator's real edit pipeline: a
//! high-fanout edit (one plan input read by 2,000 steps) is held at the preparation barrier
//! three times while an independent status write moves the project's execution-state witness
//! between releases. It is refused busy and retryable after its third stale preparation,
//! nothing is written, no preparation runs on the writer's thread, and each independent write
//! commits while a preparation waits. Its own test binary: the barrier and the counters are
//! process-wide.
#[path = "../../../tests/support/home.rs"]
mod home;
use serde_json::{Value, json};
use sluice_model::{
    commands::*,
    cost::{self, Measurement, PreparationPoint},
    error::PublicError,
    rpc::{FnInvocation, JsonMap, decode_json},
};
use sluice_process::guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence};
use sluice_runtime::{
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
};
use std::{
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Fake;
impl FnHost for Fake {
    async fn invoke(&self, _: FnInvocation) -> Result<JsonMap, PublicError> {
        Ok(JsonMap::default())
    }
}
impl AdoptionHost for Fake {
    async fn reconcile(&self, _: &AdoptionAttempt) -> std::io::Result<GuardianPresence> {
        Ok(GuardianPresence::Ambiguous("fixture".into()))
    }
}
impl ExecutionHost for Fake {
    async fn launch(&self, _: Launch) -> Result<LaunchOutcome, PublicError> {
        Ok(LaunchOutcome::Accepted)
    }
    async fn cleanup_valid(
        &self,
        _: &sluice_process::journal::CompletionJournal,
    ) -> Result<bool, PublicError> {
        Ok(true)
    }
}
fn request(name: &str, args: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&json!({"command":name,"args":args})).unwrap()).unwrap()
}
fn data(reply: CommandReply) -> Value {
    serde_json::to_value(&reply).unwrap()["data"].take()
}

/// How long an independent small write may wait while an edit is being prepared (today's
/// bound; plan-rows §11 proposes 100 ms once lane H has measured it).
const INDEPENDENT_WRITE: Duration = Duration::from_millis(750);
const READERS: usize = 2000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_high_fanout_edit_held_three_times_is_refused_busy_and_writes_nothing() {
    let home = home::ScratchHome::new().unwrap();
    assert!(home.root().exists());
    home::ScratchHome::validate(home.path()).unwrap();
    let b = Coordinator::open(home.path().into(), Catalog::fixtures(), Fake)
        .await
        .unwrap();
    let CommandReply::Project(p) = b
        .command(request(
            "project_create",
            json!({"name":"p","description":"","resources":{}}),
        ))
        .await
        .unwrap()
    else {
        panic!("project")
    };
    let project = json!({"kind":"id","value":p.project_id});
    let mut ops = vec![
        json!({"op":"input.put","name":"n","declaration":"int"}),
        json!({"op":"step.add","step":"other","spec":{"run":"core.external","outputs":{"ok":"int"}}}),
    ];
    ops.extend((0..READERS).map(|i| {
        json!({"op":"step.add","step":format!("r{i}"),"spec":{"run":"fixture.echo","in":{"value":{"source":"n"}}}})
    }));
    b.command(request(
        "plan_edit",
        json!({"project":project,"ops":ops,"start":false,"reason":"seed"}),
    ))
    .await
    .unwrap();
    let plan_rev = |b: Coordinator<Fake>, project: Value| async move {
        data(
            b.command(request("plan_get", json!({"project":project})))
                .await
                .unwrap(),
        )["rev"]
            .as_u64()
            .unwrap()
    };
    let history = |b: Coordinator<Fake>, project: Value| async move {
        data(
            b.command(request(
                "plan_history",
                json!({"project":project,"limit":1000}),
            ))
            .await
            .unwrap(),
        )["entries"]
            .as_array()
            .unwrap()
            .len()
    };
    let rev = plan_rev(b.clone(), project.clone()).await;
    let entries = history(b.clone(), project.clone()).await;

    let arrived = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (a, r) = (arrived.clone(), release.clone());
    // The only edit this binary prepares from here on is the one held.
    let hook = cost::hook_preparations(move |_: &PreparationPoint| {
        a.wait();
        r.wait();
    });
    let measurement = Measurement::start();
    // Retyping the input every reader reads: its preparation re-checks all 2,000 of them.
    let edit = tokio::spawn({
        let (b, project) = (b.clone(), project.clone());
        async move {
            b.command(request(
                "plan_edit",
                json!({"project":project,"reason":"widen","ops":[
                    {"op":"input.put","name":"n","declaration":"Any"}
                ]}),
            ))
            .await
        }
    });
    let wait = |barrier: Arc<Barrier>| tokio::task::spawn_blocking(move || barrier.wait());
    for round in 0..3 {
        wait(arrived.clone()).await.unwrap();
        // While the preparation waits: an independent status write commits at once and moves
        // the execution-state witness.
        let started = Instant::now();
        b.command(request(
            "step_set_output",
            json!({"project":project,"step":"other","outputs":{"ok":round},"force":true,"reason":"tick"}),
        ))
        .await
        .unwrap();
        let took = started.elapsed();
        assert!(
            took < INDEPENDENT_WRITE,
            "round {round}: an independent write waited {took:?} behind a preparation"
        );
        wait(release.clone()).await.unwrap();
    }
    let refused = edit.await.unwrap().unwrap_err();
    drop(hook);
    assert_eq!(
        refused,
        PublicError::Busy {
            message: "the plan's state kept changing while this edit was prepared (3 tries); send it again".into(),
            retryable: true,
        }
    );
    let counts = measurement.costs();
    assert_eq!(counts.writer_preparations, 0);
    assert_eq!(
        (
            counts.preparations.stale,
            counts.preparations.contended,
            counts.preparations.committed
        ),
        (2, 1, 0)
    );
    // Nothing was written: the revision and the history stand.
    assert_eq!(plan_rev(b.clone(), project.clone()).await, rev);
    assert_eq!(history(b.clone(), project.clone()).await, entries);
    let plan = data(
        b.command(request("plan_get", json!({"project":project})))
            .await
            .unwrap(),
    );
    assert_eq!(plan["plan"]["inputs"]["n"], "int");
}
