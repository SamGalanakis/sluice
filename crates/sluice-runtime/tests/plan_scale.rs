//! A small edit to a large plan costs the coordinator a small, bounded time, driven through
//! the coordinator socket: on a lash-shaped plan of 1,980 steps (330 units of fork, work,
//! land, landed, close and rm, with handoffs, step gates and unit gates) a local metadata or
//! binding edit answers within 100 ms in a debug build and a six-step unit addition within
//! 200 ms (`docs/design/plan-rows.md` §11). A write made while an edit is being prepared
//! does not wait for it: a preparation barrier holds the edit, the write commits within
//! 100 ms, and the edit, finding the state moved, is prepared again and commits. Two edits
//! prepared from one revision both commit, one prepared again; an edit that names the old
//! revision is refused as a conflict.
//!
//! Schema 3's edits cost what they change (rows, not the document), so these are the
//! contract's budgets, where schema 1's were 1,000, 2,500 and 750 ms. It needs the
//! integration group (B, C, D and E) and is ignored until `rw/pn-cutover` holds it. Run
//! there with `cargo test -p sluice-runtime --test plan_scale -- --include-ignored`; if the
//! integration runner's measurement says otherwise, §11 keeps the old bounds until it does.
#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
use serde_json::{Map, Value, json};
use sluice_model::{
    commands::*,
    cost::{self, Measurement, PreparationPoint},
    error::PublicError,
    ids::*,
    rpc::{FnInvocation, JsonMap, decode_json},
};
use sluice_process::guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence};
use sluice_runtime::{
    client::CoordinatorClient,
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

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

const UNITS: usize = 330;
const SUFFIXES: [&str; 6] = ["fork", "work", "land", "landed", "close", "rm"];
/// A local metadata or binding edit with a bounded affected set (§11).
const LOCAL_EDIT_BUDGET: Duration = Duration::from_millis(100);
/// A six-step unit addition with a small external boundary (§11).
const UNIT_ADD_BUDGET: Duration = Duration::from_millis(200);
/// An independent small write during an edit's preparation (§11).
const INDEPENDENT_WRITE_BUDGET: Duration = Duration::from_millis(100);

/// The steps of one lash-shaped unit: a chain of handoffs and step gates inside the
/// unit, its entry gated on the previous unit.
fn unit(name: &str, previous: Option<&str>) -> Map<String, Value> {
    let mut steps = Map::new();
    for (index, suffix) in SUFFIXES.iter().enumerate() {
        let mut step = json!({
            "run": "fixture.echo",
            "doc": format!("{suffix} for {name}, a line of prose as plans carry"),
            "tags": [format!("unit:{name}"), "lane"],
        });
        if index == 0 {
            step["in"] =
                json!({"value": {"default": {"issue": name, "branch": format!("rw/{name}")}}});
            if let Some(previous) = previous {
                step["after"] = json!([format!("unit:{previous}?")]);
            }
        } else {
            let source = format!("{name}-{}", SUFFIXES[index - 1]);
            step["in"] = json!({"value": {"source": format!("{source}/value")}});
            step["after"] = json!([source]);
        }
        if *suffix == "rm" {
            step["tags"] = json!([format!("unit:{name}"), "lane", "exit"]);
        }
        steps.insert(format!("{name}-{suffix}"), step);
    }
    steps
}

struct Fixture {
    _home: home::ScratchHome,
    client: CoordinatorClient,
    project: ProjectId,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), PublicError>>,
}
fn payload(reply: CommandReply) -> Value {
    let value = serde_json::to_value(&reply).unwrap();
    value.get("data").cloned().unwrap_or(value)
}
impl Fixture {
    async fn new() -> Self {
        let home = home::ScratchHome::new().unwrap();
        home::ScratchHome::validate(home.path()).unwrap();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), Fake)
            .await
            .unwrap();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let server = tokio::spawn(async move { broker.serve(token).await });
        let client = CoordinatorClient::new(home.path());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !client.path.exists() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let created = client
            .command(request(
                "project_create",
                json!({"name":"lash","description":"","resources":{}}),
            ))
            .await
            .unwrap();
        let project = serde_json::from_value(payload(created)["project_id"].clone()).unwrap();
        let recipes = home
            .path()
            .join("projects")
            .join(format!("{project}"))
            .join("recipes");
        std::fs::create_dir_all(&recipes).unwrap();
        let mut steps = Map::new();
        for (key, mut step) in unit("{unit}", None) {
            step.as_object_mut().unwrap().remove("tags");
            steps.insert(key, step);
        }
        std::fs::write(
            recipes.join("lane.json"),
            serde_json::to_vec(&json!({"name":"lane","steps":steps})).unwrap(),
        )
        .unwrap();
        Self {
            _home: home,
            client,
            project,
            stop,
            server,
        }
    }
    async fn call(&self, name: &str, mut args: Value) -> Result<Value, PublicError> {
        args["project"] = json!({"kind":"id","value":self.project});
        self.client.command(request(name, args)).await.map(payload)
    }
    async fn rev(&self) -> u64 {
        self.call("plan_get", json!({})).await.unwrap()["rev"]
            .as_u64()
            .unwrap()
    }
    /// One edit, timed from the client: its reply is an edit result at a new revision (or,
    /// for a typed tool whose lowering changes nothing, at the same one).
    async fn timed(&self, name: &str, args: Value) -> Duration {
        let before = self.rev().await;
        let started = Instant::now();
        let reply = self.call(name, args).await.unwrap();
        let took = started.elapsed();
        let rev = reply["rev"]
            .as_u64()
            .unwrap_or_else(|| panic!("{name}: {reply}"));
        assert!(rev >= before, "{name}: {reply}");
        assert_eq!(reply["preview"]["scope"], "impact", "{name}: {reply}");
        took
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
    }
}
fn request(name: &str, args: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&json!({"command":name,"args":args})).unwrap()).unwrap()
}
fn edit(reason: &str) -> Value {
    json!({"dry_run":false,"reason":reason})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "plan-rows integration gate: run on rw/pn-cutover (needs lanes B, C, D and E)"]
async fn small_edits_to_a_plan_of_two_thousand_steps_take_a_bounded_time() {
    let f = Fixture::new().await;
    let mut ops = Vec::new();
    let mut previous: Option<String> = None;
    for index in 0..UNITS {
        let name = format!("fig-{}", 4000 + index);
        for (step, spec) in unit(&name, previous.as_deref()) {
            ops.push(json!({"op": "step.add", "step": step, "spec": spec}));
        }
        previous = Some(name);
    }
    assert_eq!(ops.len(), UNITS * SUFFIXES.len());
    // The whole plan in one edit, every step added paused so nothing is launched.
    f.call(
        "plan_edit",
        json!({"ops": ops, "start": false, "reason": "seed"}),
    )
    .await
    .unwrap();
    // A first edit warms the plan cache, as a running coordinator's would be.
    f.call(
        "plan_edit",
        json!({"ops": [{"op": "step.update", "step": "fig-4000-fork", "changes": {"doc": "warm"}}],
            "reason": "warm"}),
    )
    .await
    .unwrap();

    let measurement = Measurement::start();
    let mut took = vec![];
    took.push((
        "plan_edit metadata",
        f.timed(
            "plan_edit",
            json!({"ops": [{"op": "step.update", "step": "fig-4140-rm",
                "changes": {"tags": ["unit:fig-4140", "lane", "exit", "probe"]}}],
                "reason": "probe"}),
        )
        .await,
    ));
    took.push((
        "plan_edit binding",
        f.timed(
            "plan_edit",
            json!({"ops": [{"op": "step.update", "step": "fig-4141-work",
                "changes": {"in": {"value": {"source": "fig-4141-fork/value.issue"}}}}],
                "reason": "rebind"}),
        )
        .await,
    ));
    took.push((
        "step_add",
        f.timed(
            "step_add",
            json!({"step":"extra","spec":{"run":"fixture.echo","in":{"value":{"source":"fig-4140-rm/value"}},"after":["unit:fig-4140"]},
                "start":false,"edit":edit("add")}),
        )
        .await,
    ));
    took.push((
        "edge_add",
        f.timed(
            "edge_add",
            json!({"step":"extra","after":["fig-4200-land"],"edit":edit("gate")}),
        )
        .await,
    ));
    took.push((
        "unit_add",
        f.timed(
            "unit_add",
            json!({"recipe":"lane","unit":"fig-9000","params":{},"after":{"*":["unit:fig-4329?"]},
                "start":false,"edit":edit("unit")}),
        )
        .await,
    ));
    took.push((
        "step_remove",
        f.timed(
            "step_remove",
            json!({"selection":{"steps":["extra"],"tags":null},"edit":edit("remove")}),
        )
        .await,
    ));
    took.push((
        "plan_prune",
        f.timed(
            "plan_prune",
            json!({"units":null,"tags":null,"older_than_seconds":0,"edit":edit("prune")}),
        )
        .await,
    ));
    let costs = measurement.costs();
    drop(measurement);
    eprintln!(
        "edit times on {} steps: {took:?}; costs {costs:?}",
        UNITS * SUFFIXES.len()
    );
    assert_eq!(
        (
            costs.full_exports,
            costs.full_compiles,
            costs.writer_preparations
        ),
        (0, 0, 0),
        "no edit on the warm cache exported or compiled the plan: {costs:?}"
    );
    for (name, one) in &took {
        let budget = if *name == "unit_add" {
            UNIT_ADD_BUDGET
        } else {
            LOCAL_EDIT_BUDGET
        };
        assert!(
            *one < budget,
            "{name} on a plan of {} steps took {one:?} (budget {budget:?}); every edit: {took:?}",
            UNITS * SUFFIXES.len(),
        );
    }

    // A write made while an edit is being prepared does not wait for it. The barrier holds
    // the edit's first preparation (no sleep: the write is issued only once the edit is
    // there); the write moves the board's revision, a token the edit was prepared from, so
    // the edit is prepared again and commits.
    let rev = f.rev().await;
    let (reached, at_barrier) = std::sync::mpsc::channel::<usize>();
    let (release, go) = std::sync::mpsc::channel::<()>();
    let go = Arc::new(Mutex::new(go));
    let project = f.project;
    let hook = cost::hook_preparations(move |point: &PreparationPoint| {
        if point.project == project && point.attempt == 1 {
            reached.send(point.attempt).unwrap();
            go.lock().unwrap().recv().unwrap();
        }
    });
    let measurement = Measurement::start();
    let held = {
        let client = f.client.clone();
        let project = f.project;
        tokio::spawn(async move {
            client
                .command(request(
                    "plan_edit",
                    json!({"project": {"kind": "id", "value": project}, "reason": "held",
                        "ops": [{"op": "step.update", "step": "fig-4141-rm",
                            "changes": {"tags": ["unit:fig-4141", "lane", "exit", "held"]}}]}),
                ))
                .await
        })
    };
    tokio::task::spawn_blocking(move || at_barrier.recv_timeout(Duration::from_secs(60)))
        .await
        .unwrap()
        .expect("the edit reaches its preparation barrier");
    let started = Instant::now();
    f.call(
        "board_set",
        json!({"program":"root = Doc(\"set while an edit is prepared\")"}),
    )
    .await
    .unwrap();
    let slot = started.elapsed();
    release.send(()).unwrap();
    let held = payload(held.await.unwrap().unwrap());
    drop(hook);
    assert_eq!(held["rev"].as_u64(), Some(rev + 1), "{held}");
    let costs = measurement.costs();
    drop(measurement);
    assert!(costs.preparations.stale >= 1, "prepared again: {costs:?}");
    assert_eq!(costs.writer_preparations, 0, "{costs:?}");
    eprintln!("a board set during an edit's preparation took {slot:?}");
    assert!(
        slot < INDEPENDENT_WRITE_BUDGET,
        "a write during an edit took {slot:?}"
    );

    // Two edits prepared from one revision both commit, one of them prepared again; an edit
    // naming the old revision is refused as a conflict.
    let rev = f.rev().await;
    let add = |step: &str| {
        json!({"step":step,"spec":{"run":"fixture.echo","in":{"value":{"default":step}}},
            "start":false,"edit":edit("race")})
    };
    let (first, second) = tokio::join!(
        f.call("step_add", add("race-a")),
        f.call("step_add", add("race-b")),
    );
    let mut revs = vec![
        first.unwrap()["rev"].as_u64().unwrap(),
        second.unwrap()["rev"].as_u64().unwrap(),
    ];
    revs.sort();
    assert_eq!(revs, [rev + 1, rev + 2]);
    let stale = f
        .call(
            "plan_edit",
            json!({"rev": rev, "reason": "race",
                "ops": [{"op": "step.add", "step": "race-c", "spec": {"run": "fixture.echo",
                    "in": {"value": {"default": 1}}, "paused": true}}]}),
        )
        .await;
    match stale {
        Err(PublicError::Conflict { current_rev, .. }) => {
            assert_eq!(current_rev, Some(Revision(rev + 2)));
        }
        other => panic!("plan_edit at rev {rev}: {other:?}"),
    }
    assert_eq!(f.rev().await, rev + 2);
    f.close().await;
}
